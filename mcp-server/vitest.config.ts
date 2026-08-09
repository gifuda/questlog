import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    // Pure-logic tests only — Node environment, no DOM needed.
    environment: "node",
    include: ["test/**/*.test.ts"],
    // Redirect the MCP registry to an isolated temp file so tests never
    // pollute the user's real ~/.todo-board.json with test temp-dirs.
    globalSetup: ["./test/global-setup.ts"],
  },
});
