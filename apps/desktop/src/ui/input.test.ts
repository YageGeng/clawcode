import { expect, test } from "nsh/test";

import { textareaHeights } from "./input";

test("textarea_bounds_follow_the_type_scale", () => {
  // 2 and 8 lines of 20pt UI text plus 16pt vertical padding.
  expect(textareaHeights()).toEqual({ min: 56, max: 176 });
  expect(textareaHeights(1, 4)).toEqual({ min: 36, max: 96 });
});
