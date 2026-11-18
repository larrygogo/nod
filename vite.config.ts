import { defineConfig } from "vite";
import preact from "@preact/preset-vite";

// Tauri expects a fixed port and fails if it is not available.
export default defineConfig({
  root: "ui",
  plugins: [preact()],
  clearScreen: false,
  server: { port: 1430, strictPort: true, watch: { ignored: ["**/src-tauri/**"] } },
  build: { outDir: "../dist", emptyOutDir: true, target: "es2021" },
});
