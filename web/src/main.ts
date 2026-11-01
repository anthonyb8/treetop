import { mount } from "svelte";
import App from "./App.svelte";
import "./app.css";

const target = document.getElementById("app");
if (!target) throw new Error("no #app element");
mount(App, { target });

// The service worker keeps the app's own files, so it opens from the home
// screen even while the server is out of reach. Vite's dev server has none.
if ("serviceWorker" in navigator && import.meta.env.PROD) {
  void navigator.serviceWorker.register("/sw.js");
}
