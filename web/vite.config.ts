import { defineConfig } from "vite";

// Served from https://<user>.github.io/moqdesk/, so assets must be relative.
export default defineConfig({ base: "./", build: { target: "es2022" } });
