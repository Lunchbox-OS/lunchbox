/**
 * Entry point for the standalone static build.
 *
 * Deliberately thin, and deliberately importing nothing from `src/api/`: this
 * bundle is served from a static host with no daemon anywhere near it. The
 * boundary is enforced by `scripts/check-boundary.mjs`.
 */
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import CssBaseline from "@mui/material/CssBaseline";
import { ThemeProvider } from "@mui/material/styles";
import "@fontsource/roboto/400.css";
import "@fontsource/roboto/500.css";
import "@fontsource/roboto/700.css";
import theme from "../theme";
import { ConfigApp } from "./ConfigApp";

const container = document.getElementById("root");
if (!container) throw new Error("no #root element");

createRoot(container).render(
  <StrictMode>
    <ThemeProvider theme={theme}>
      <CssBaseline />
      <ConfigApp />
    </ThemeProvider>
  </StrictMode>,
);
