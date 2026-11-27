// P8.3b e2e entry: install the mocked IPC first, then mount the real <App/>.
// Not StrictMode — we want clean, single IPC call counts for assertions.
import "./install-mocks";
import { createRoot } from "react-dom/client";
import App from "../../../src/App";
import "../../../src/styles.css";

createRoot(document.getElementById("root")!).render(<App />);
