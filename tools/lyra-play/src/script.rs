//! The `--script` command-script parser.
//!
//! A script is one command per line. Blank lines and `#` comments are ignored.
//! Commands drive the engine deterministically so QA can replay gapless, seek and
//! command-storm scenarios headless:
//!
//! ```text
//! play                 # start / resume playback
//! pause                # pause (device stays open)
//! stop                 # stop
//! next                 # next queue entry
//! prev                 # previous entry (or restart the first)
//! seek <frame>         # seek the current track to a frame
//! playat <i>           # jump to queue entry i (0-based)
//! wait <ms>            # sleep, still draining events
//! wait-eos             # block until playback stops at the end of the queue
//! quit                 # end the script (and the run)
//! ```

use lyra_engine::EngineCommand;

/// A parsed script step: an engine command, or player-side control flow.
#[derive(Debug, Clone, PartialEq)]
pub enum ScriptCmd {
    /// Forward this command to the engine.
    Engine(EngineCommand),
    /// Sleep for this many milliseconds (still draining events).
    Wait(u64),
    /// Block until the engine reaches the end of the queue.
    WaitEos,
    /// End the script.
    Quit,
}

/// Parse a whole script into steps, reporting the 1-based line number on error.
pub fn parse_script(text: &str) -> Result<Vec<ScriptCmd>, String> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        match parse_line(line) {
            Ok(Some(cmd)) => out.push(cmd),
            Ok(None) => {}
            Err(e) => return Err(format!("line {}: {e}", i + 1)),
        }
    }
    Ok(out)
}

/// Parse one line into an optional step (blank/comment lines yield `None`).
pub fn parse_line(line: &str) -> Result<Option<ScriptCmd>, String> {
    // Strip a trailing comment and surrounding whitespace.
    let line = line.split('#').next().unwrap_or("").trim();
    if line.is_empty() {
        return Ok(None);
    }
    let mut it = line.split_whitespace();
    let verb = it.next().unwrap().to_ascii_lowercase();
    let arg = it.next();
    if it.next().is_some() {
        return Err(format!("too many arguments for `{verb}`"));
    }

    let cmd = match verb.as_str() {
        "play" => ScriptCmd::Engine(EngineCommand::Play),
        "pause" => ScriptCmd::Engine(EngineCommand::Pause),
        "stop" => ScriptCmd::Engine(EngineCommand::Stop),
        "next" => ScriptCmd::Engine(EngineCommand::Next),
        "prev" => ScriptCmd::Engine(EngineCommand::Prev),
        "quit" | "exit" => ScriptCmd::Quit,
        "wait-eos" | "waiteos" => ScriptCmd::WaitEos,
        "seek" => ScriptCmd::Engine(EngineCommand::Seek(parse_u64(verb.as_str(), arg)?)),
        "playat" => ScriptCmd::Engine(EngineCommand::PlayAt(parse_usize(verb.as_str(), arg)?)),
        "wait" => ScriptCmd::Wait(parse_u64(verb.as_str(), arg)?),
        other => return Err(format!("unknown command `{other}`")),
    };
    Ok(Some(cmd))
}

fn parse_u64(verb: &str, arg: Option<&str>) -> Result<u64, String> {
    arg.ok_or_else(|| format!("`{verb}` needs a number"))?
        .parse::<u64>()
        .map_err(|_| format!("`{verb}`: expected a non-negative integer"))
}

fn parse_usize(verb: &str, arg: Option<&str>) -> Result<usize, String> {
    arg.ok_or_else(|| format!("`{verb}` needs an index"))?
        .parse::<usize>()
        .map_err(|_| format!("`{verb}`: expected a non-negative index"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_verbs() {
        let text = "\
# a scripted run
play
seek 44100
pause
wait 250
next
playat 2
prev
wait-eos
stop
quit
";
        let cmds = parse_script(text).expect("valid script");
        assert_eq!(
            cmds,
            vec![
                ScriptCmd::Engine(EngineCommand::Play),
                ScriptCmd::Engine(EngineCommand::Seek(44_100)),
                ScriptCmd::Engine(EngineCommand::Pause),
                ScriptCmd::Wait(250),
                ScriptCmd::Engine(EngineCommand::Next),
                ScriptCmd::Engine(EngineCommand::PlayAt(2)),
                ScriptCmd::Engine(EngineCommand::Prev),
                ScriptCmd::WaitEos,
                ScriptCmd::Engine(EngineCommand::Stop),
                ScriptCmd::Quit,
            ]
        );
    }

    #[test]
    fn blank_and_comment_lines_are_skipped() {
        let cmds = parse_script("\n   \n# just a comment\nplay   # inline comment\n").unwrap();
        assert_eq!(cmds, vec![ScriptCmd::Engine(EngineCommand::Play)]);
    }

    #[test]
    fn case_insensitive_verbs() {
        assert_eq!(parse_line("PLAY").unwrap(), Some(ScriptCmd::Engine(EngineCommand::Play)));
        assert_eq!(parse_line("Seek 10").unwrap(), Some(ScriptCmd::Engine(EngineCommand::Seek(10))));
    }

    #[test]
    fn errors_report_the_line_number() {
        let err = parse_script("play\nseek\n").unwrap_err();
        assert!(err.contains("line 2"), "{err}");
        assert!(err.contains("needs a number"), "{err}");
    }

    #[test]
    fn rejects_unknown_and_extra_args() {
        assert!(parse_line("frobnicate").is_err());
        assert!(parse_line("seek 1 2").is_err());
        assert!(parse_line("seek notanumber").is_err());
    }
}
