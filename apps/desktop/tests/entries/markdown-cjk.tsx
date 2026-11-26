import { mount, Text, View } from "nsh";

import { configureDesktopFonts } from "../../src/ui/fonts";
import { Markdown } from "../../src/ui/markdown";

const sample = `## 变更说明

| 文件 | 审查结果 |
| --- | --- |
| \`Cargo.toml\` | 中文正文与 \`nsh\` 0.7.1 混排。 |
| 配置 | **修复完成**，可以复核。 |

行内代码：\`中文代码\`。`;

/** Exercises CJK font fallback in ordinary labels and rich text in the same native window. */
function Entry() {
  return (
    <View class="flex-1 gap-4 bg-background p-6">
      <Text class="text-foreground">会话记录 · Session</Text>
      <Markdown text={sample} />
    </View>
  );
}

configureDesktopFonts();
mount(Entry);
