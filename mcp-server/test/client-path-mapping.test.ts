import { describe, it, expect, afterEach } from "vitest";
import { mapClientPath, effectiveCwd } from "../src/store.js";
import os from "node:os";

describe("client path mapping (ssh wrapper MCP_CLIENT_CWD)", () => {
  const orig = process.env.MCP_CLIENT_CWD;
  afterEach(() => {
    if (orig === undefined) delete process.env.MCP_CLIENT_CWD;
    else process.env.MCP_CLIENT_CWD = orig;
  });

  it("returns null and falls back to process.cwd() when env unset", () => {
    delete process.env.MCP_CLIENT_CWD;
    expect(mapClientPath("/Users/abo/x")).toBeNull();
    expect(effectiveCwd()).toBe(process.cwd());
  });

  it("maps a client-home path onto this host's home", () => {
    process.env.MCP_CLIENT_CWD = "/Users/abo/some/proj";
    expect(mapClientPath("/Users/abo/some/proj")).toBe(os.homedir() + "/some/proj");
    expect(mapClientPath("/Users/abo")).toBe(os.homedir());
    expect(mapClientPath("/Users/abo")).not.toBe("/Users/power");
  });

  it("rejects paths outside the client home prefix", () => {
    process.env.MCP_CLIENT_CWD = "/Users/abo/some/proj";
    expect(mapClientPath("/etc/passwd")).toBeNull();
    expect(mapClientPath("/Users/evil/x")).toBeNull();
  });

  it("falls back to process.cwd() for non-absolute env values", () => {
    process.env.MCP_CLIENT_CWD = "relative/path";
    expect(effectiveCwd()).toBe(process.cwd());
  });
});
