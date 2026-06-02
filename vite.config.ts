import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

export default defineConfig(async () => {
  const { version } = await import('./package.json');

  return {
    plugins: [vue()],

    define: {
      __APP_VERSION__: JSON.stringify(version),
    },

    clearScreen: false,
    server: {
      port: 1431,
      strictPort: true,
      host: host || false,
      hmr: host
        ? { protocol: "ws", host, port: 1432 }
        : undefined,
      watch: {
        ignored: ["**/src-tauri/**"],
      },
    },
  };
});
