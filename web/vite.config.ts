import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

// `npm run dev` serves the app on 5173 with live reload and passes /api to a
// `treetop serve` on 7777, so `tether share 5173` puts the app being built on
// a phone. Shared, the page is HTTPS on the same port, so the reload socket
// goes there too, and the tailnet's *.ts.net name has to be let in.
const api = process.env.TREETOP_API ?? "http://127.0.0.1:7777";

export default defineConfig({
  plugins: [svelte()],
  server: {
    host: "127.0.0.1",
    port: 5173,
    strictPort: true,
    allowedHosts: [".ts.net"],
    hmr: process.env.TREETOP_SHARED ? { protocol: "wss", clientPort: 5173 } : undefined,
    proxy: { "/api": { target: api, changeOrigin: false } },
  },
  build: { outDir: "dist", emptyOutDir: true, assetsDir: "assets" },
});
