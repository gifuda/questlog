import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    // Pure-logic tests only — Node environment, no DOM needed.
    environment: "node",
    include: ["test/**/*.test.ts"],
  },
});
