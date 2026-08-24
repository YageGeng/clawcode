import react from "@vitejs/plugin-react-swc";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [react()],
  build: {
    rolldownOptions: {
      output: {
        codeSplitting: {
          groups: [
            {
              // Keep KaTeX out of the lazy Markdown renderer chunk to limit its parse cost.
              name: "katex",
              test: /node_modules[\\/]katex[\\/]/
            }
          ]
        }
      }
    }
  },
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
