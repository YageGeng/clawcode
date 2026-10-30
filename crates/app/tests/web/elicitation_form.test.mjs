import assert from "node:assert/strict";
import test from "node:test";

import { ElicitationForm } from "../../../../web/src/features/mcp/elicitationForm.ts";

/** JSON Schema length counts Unicode code points, including supplementary characters once. */
test("maxLength accepts emoji through its code-point boundary", () => {
  const form = ElicitationForm.parse({
    type: "object",
    properties: { note: { type: "string", maxLength: 40 } }
  });
  for (const note of ["🙂".repeat(21), "🙂".repeat(40), "中文🙂".repeat(13)]) {
    assert.deepEqual(form.validate({ note }), { errors: {}, content: { note } });
  }
  assert.equal(form.validate({ note: "🙂".repeat(41) }).content, undefined);
});

/** A surrogate pair must not satisfy a two-code-point minimum by itself. */
test("minLength rejects one emoji and accepts two", () => {
  const form = ElicitationForm.parse({
    type: "object",
    properties: { note: { type: "string", minLength: 2 } }
  });
  assert.equal(form.validate({ note: "🙂" }).content, undefined);
  assert.deepEqual(form.validate({ note: "🙂🙂" }).content, { note: "🙂🙂" });
});

/** Combining marks remain separate code points even when rendered as one grapheme. */
test("length limits count combining marks without changing ASCII validation", () => {
  const form = ElicitationForm.parse({
    type: "object",
    properties: { note: { type: "string", maxLength: 1 } }
  });
  assert.equal(form.validate({ note: "e\u0301" }).content, undefined);
  assert.equal(form.validate({ note: "ab" }).content, undefined);
  assert.deepEqual(form.validate({ note: "a" }).content, { note: "a" });
});
