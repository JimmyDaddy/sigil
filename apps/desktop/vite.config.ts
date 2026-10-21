import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    host: "127.0.0.1",
    port: 1420,
    strictPort: true,
  },
  envPrefix: ["VITE_"],
  build: {
    target: ["es2022", "chrome105", "safari13"],
    sourcemap: true,
    // Keep fonts same-origin for the desktop font-src 'self' policy.
    assetsInlineLimit: (filePath) =>
      /\.(?:woff2?|ttf|otf|eot)$/i.test(filePath) ? false : undefined,
  },
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.ts", "src/**/*.test.tsx"],
    setupFiles: ["./src/test-setup.ts"],
  },
});
