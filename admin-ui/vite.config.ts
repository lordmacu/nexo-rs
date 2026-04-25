import { defineConfig } from "vite";
import react from "@vitejs/plugin-react-swc";

// Build targets a single-page app served by the Rust `agent admin`
// subcommand. We pin relative asset URLs so the bundle can be served
// from whatever subpath the operator exposes; in practice `agent
// admin` serves at `/`, but having `base: "./"` means serving from
// `/admin/` later is a one-line flip.
export default defineConfig({
  plugins: [react()],
  base: "./",
  build: {
    outDir: "dist",
    emptyOutDir: true,
    sourcemap: false,
    // Keep chunks inline — the agent binary embeds every file
    // literally; fewer files = smaller embedded directory walk.
    rollupOptions: {
      output: {
        manualChunks: undefined,
      },
    },
  },
  server: {
    port: 5173,
    strictPort: true,
  },
});
