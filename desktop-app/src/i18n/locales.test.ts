/**
 * i18n dictionary integrity tests.
 *
 * Guards against the two failure modes that shipped once already:
 * 1. zh/en key drift — a key missing from one locale renders as the raw key
 *    (e.g. "card.cancel") in the UI.
 * 2. Code referencing a key that doesn't exist in the dictionaries.
 */
import { describe, expect, it } from "vitest";
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import zh from "./locales/zh.json";

type Dict = Record<string, unknown>;

function flatten(d: Dict, prefix = ""): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(d)) {
    const key = prefix ? `${prefix}.${k}` : k;
    if (typeof v === "object" && v !== null) {
      Object.assign(out, flatten(v as Dict, key));
    } else {
      out[key] = String(v);
    }
  }
  return out;
}

const zhFlat = flatten(zh as Dict);


describe("i18n dictionaries", () => {
  it("every value is a non-empty string", () => {
    for (const [k, v] of Object.entries(zhFlat)) {
      expect(v.trim().length, `zh:${k} is empty`).toBeGreaterThan(0);
    }
  });

  it("all t('...') calls in src reference existing keys", () => {
    const here = dirname(fileURLToPath(import.meta.url));
    const srcDir = join(here, "..", "..", "src");
    const files: string[] = [];
    const walk = (dir: string) => {
      for (const name of readdirSync(dir, { withFileTypes: true })) {
        const p = join(dir, name.name);
        if (name.isDirectory()) walk(p);
        else if (name.name.endsWith(".ts") || name.name.endsWith(".tsx")) files.push(p);
      }
    };
    walk(srcDir);

    const literalKeys = new Set<string>();
    const keyMapValues = new Set<string>();
    for (const f of files) {
      const text = readFileSync(f, "utf8");
      // t("literal.key") — string literals
      for (const m of text.matchAll(/t\("([a-z][a-zA-Z0-9.]*)"\s*[,)]/g)) {
        literalKeys.add(m[1]);
      }
      // : "card.itemTypeTodo" style key-map values (Record<string, string>)
      for (const m of text.matchAll(/:\s*"([a-z]+\.[a-zA-Z]+)"/g)) {
        keyMapValues.add(m[1]);
      }
    }

    const namespaces = new Set(Object.keys(zhFlat).map((k) => k.split(".")[0]));
    const missing = [...literalKeys, ...keyMapValues]
      .filter((k) => namespaces.has(k.split(".")[0]) && !zhFlat[k]);

    expect(missing).toEqual([]);
  });
});
