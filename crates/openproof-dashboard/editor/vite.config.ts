import { defineConfig, normalizePath } from "vite";
import react from "@vitejs/plugin-react";
import { nodePolyfills } from "vite-plugin-node-polyfills";
import importMetaUrlPlugin from "@codingame/esbuild-import-meta-url-plugin";
import { viteStaticCopy } from "vite-plugin-static-copy";
import path from "node:path";

export default defineConfig({
  base: "/editor/",
  build: {
    outDir: "../static/editor-dist",
    emptyDirBeforeBuild: true,
    rollupOptions: {
      output: {
        entryFileNames: "assets/bundle.js",
        chunkFileNames: "assets/[name].js",
        assetFileNames: "assets/[name].[ext]",
      },
    },
  },
  optimizeDeps: {
    esbuildOptions: {
      plugins: [importMetaUrlPlugin],
    },
  },
  plugins: [
    react(),
    nodePolyfills({ overrides: { fs: "memfs" } }),
    viteStaticCopy({
      targets: [
        {
          src: [
            normalizePath(
              path.resolve(
                __dirname,
                "node_modules/@leanprover/infoview/dist/*"
              )
            ),
            normalizePath(
              path.resolve(
                __dirname,
                "node_modules/lean4monaco/dist/webview/webview.js"
              )
            ),
          ],
          dest: "infoview",
        },
        {
          src: [
            normalizePath(
              path.resolve(
                __dirname,
                "node_modules/@leanprover/infoview/dist/codicon.ttf"
              )
            ),
          ],
          dest: "assets",
        },
      ],
    }),
  ],
});
