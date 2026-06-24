import React from "react";
import ReactDOM from "react-dom/client";
import { App } from "./App";
import { PetWindow } from "./PetWindow";
import "./styles.css";

const params = new URLSearchParams(window.location.search);
const Root = params.get("window") === "pet" ? PetWindow : App;

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <Root />
  </React.StrictMode>
);
