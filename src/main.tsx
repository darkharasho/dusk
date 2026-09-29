import React from "react";
import ReactDOM from "react-dom/client";
import { App } from "./App";

// The design language, bundled rather than linked from the Pages URL: a
// desktop window opened offline must still paint, and a linked stylesheet
// costs a round-trip before first paint even when it is online.
import "@axiapps/axi-design/axi.css";
import "./app.css";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
