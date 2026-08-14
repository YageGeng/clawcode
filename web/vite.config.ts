import react from "@vitejs/plugin-react-swc";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      "/api": "http://127.0.0.1:3000",
      "/acp": {
        target: "ws://127.0.0.1:3000",
        ws: true
      }
    }
  }
});
