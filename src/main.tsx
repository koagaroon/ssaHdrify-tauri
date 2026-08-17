import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./index.css";
import App from "./App.tsx";
import I18nProvider from "./i18n/I18nProvider";
import ThemeProvider from "./theme/ThemeProvider";
import { FileProvider } from "./lib/FileContext";
import StatusProvider from "./lib/StatusProvider";
import AppErrorBoundary from "./AppErrorBoundary";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <AppErrorBoundary>
      <ThemeProvider>
        <I18nProvider>
          <FileProvider>
            <StatusProvider>
              <App />
            </StatusProvider>
          </FileProvider>
        </I18nProvider>
      </ThemeProvider>
    </AppErrorBoundary>
  </StrictMode>
);
