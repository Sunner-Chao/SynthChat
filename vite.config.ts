import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  server: {
    host: "127.0.0.1",
    port: 1421,
    strictPort: true,
    watch: {
      ignored: ["**/src-tauri/target/**", "**/target/**", "**/target-codex-check/**"]
    }
  },
  clearScreen: false
});
