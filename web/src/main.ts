import { mount } from "svelte";
import App from "./App.svelte";
import "./app.css";

const target = document.getElementById("app");
if (!target) throw new Error("no #app element");
mount(App, { target });
// Started after all, later than index.html waits for: its notice is wrong now.
document.getElementById("not-started")?.remove();

// The service worker shows notifications. It keeps nothing, so every open
// loads this page from the server. Vite's dev server has none.
if ("serviceWorker" in navigator && import.meta.env.PROD) {
  void navigator.serviceWorker.register("/sw.js");
}
