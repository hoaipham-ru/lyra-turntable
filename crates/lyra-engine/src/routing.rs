//! DSD delivery selection from device capabilities (ADR-009).
//!
//! A DSD source can reach the DAC three ways; the engine chooses in this order:
//! native DSD over ASIO → DoP over PCM → DSD decimated to PCM. The preference is a
//! pure function of the device [`DeviceCaps`] and the [`OutputMode`], so P3.1 wires
//! the selection and P3.3+ extends the transports it returns. On the dev laptop's
//! built-in WASAPI codec neither native nor DoP is marked, so the choice is
//! [`DsdDelivery::ToPcm`].

use lyra_core::{DeviceCaps, OutputMode};

/// How a DSD source is carried to the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DsdDelivery {
    /// Raw 1-bit DSD to an ASIO driver that accepts `kASIODSDFormat`.
    Native,
    /// DSD-over-PCM: 16 DSD bits per channel packed into 24-bit words.
    Dop,
    /// Decimated to 24-bit PCM for a DAC with no DSD support (a *processed* path,
    /// ADR-004 — not bit-perfect).
    ToPcm,
}

/// Choose the DSD delivery for a device, per ADR-009 (native → DoP → convert).
///
/// Native DSD is only offered when the caller asked for ASIO with `native_dsd`
/// **and** the device reports it; DoP only when the device is explicitly marked
/// DoP-capable (never assumed for an unknown device). Everything else decimates to
/// PCM, the only path that works on a plain WASAPI codec.
pub(crate) fn choose_dsd_delivery(caps: &DeviceCaps, mode: OutputMode) -> DsdDelivery {
    let asio_native_requested = matches!(mode, OutputMode::Asio { native_dsd: true });
    if asio_native_requested && caps.asio && caps.native_dsd {
        DsdDelivery::Native
    } else if caps.dop {
        DsdDelivery::Dop
    } else {
        DsdDelivery::ToPcm
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(dop: bool, native: bool, asio: bool) -> DeviceCaps {
        DeviceCaps {
            name: "test".into(),
            exclusive: true,
            shared: true,
            asio,
            dop,
            native_dsd: native,
        }
    }

    #[test]
    fn wasapi_codec_falls_back_to_pcm() {
        // The dev laptop codec: no DoP, no native DSD.
        let c = caps(false, false, false);
        assert_eq!(
            choose_dsd_delivery(&c, OutputMode::WasapiExclusive),
            DsdDelivery::ToPcm
        );
    }

    #[test]
    fn dop_capable_device_uses_dop() {
        let c = caps(true, false, false);
        assert_eq!(
            choose_dsd_delivery(&c, OutputMode::WasapiExclusive),
            DsdDelivery::Dop
        );
    }

    #[test]
    fn native_preferred_when_asio_native_requested_and_supported() {
        let c = caps(true, true, true);
        assert_eq!(
            choose_dsd_delivery(&c, OutputMode::Asio { native_dsd: true }),
            DsdDelivery::Native
        );
    }

    #[test]
    fn native_not_used_without_asio_native_request() {
        let c = caps(true, true, true);
        // PCM-oriented mode: a native-capable device still gets DoP, not native.
        assert_eq!(
            choose_dsd_delivery(&c, OutputMode::WasapiExclusive),
            DsdDelivery::Dop
        );
        // ASIO but native not requested → DoP.
        assert_eq!(
            choose_dsd_delivery(&c, OutputMode::Asio { native_dsd: false }),
            DsdDelivery::Dop
        );
    }

    #[test]
    fn native_requested_but_device_lacks_it_falls_through() {
        let c = caps(false, false, true);
        assert_eq!(
            choose_dsd_delivery(&c, OutputMode::Asio { native_dsd: true }),
            DsdDelivery::ToPcm
        );
    }
}
