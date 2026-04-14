import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./index.css";
import App from "./App.tsx";
import I18nProvider from "./i18n/I18nProvider";
import ThemeProvider from "./theme/ThemeProvider";
import { FileProvider } from "./lib/FileContext";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <ThemeProvider>
      <I18nProvider>
        <FileProvider>
          <App />
        </FileProvider>
      </I18nProvider>
    </ThemeProvider>
  </StrictMode>
);
