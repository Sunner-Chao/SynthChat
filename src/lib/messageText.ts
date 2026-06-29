export function stripToolDirectiveBlocks(content: string) {
  const match = /(^|\n)\s*<(?:tool_call|tool_calls|function=|function_call|function_calls|tool_result)(?:\s|>|=)/i.exec(content);
  if (!match || match.index < 0) return content;
  return content.slice(0, match.index).trimEnd();
}

export function isAttachmentContextLine(line: string) {
  const trimmed = line.trim();
  if (!trimmed.startsWith("{") || !trimmed.includes("\"attachment\"")) return false;
  try {
    const parsed = JSON.parse(trimmed) as { type?: string };
    return parsed?.type === "attachment";
  } catch {
    return false;
  }
}

export function isMediaDirectiveLine(line: string) {
  const trimmed = line.trim();
  return trimmed.includes("[media attached:") || /^`?MEDIA:\s*(?:"[^"]+"|'[^']+'|`[^`]+`|.+)`?$/i.test(trimmed);
}

export function renderTextForMessage(content: string) {
  return stripToolDirectiveBlocks(content.trim())
    .split(/\r?\n/)
    .filter((line) => !isAttachmentContextLine(line))
    .join("\n")
    .trim();
}

export function displayTextForMessage(content: string) {
  return stripToolDirectiveBlocks(content)
    .split(/\r?\n/)
    .filter((line) => !isAttachmentContextLine(line) && !isMediaDirectiveLine(line))
    .join("\n")
    .trim();
}

export function speechTextForMessage(content: string) {
  const text = renderTextForMessage(content)
    .replace(/\[\[audio_as_voice\]\]/gi, "")
    .replace(/```[\s\S]*?```/g, " ")
    .replace(/`([^`\n]{1,120})`/g, "$1")
    .split(/\r?\n/)
    .filter((line) => !isMediaDirectiveLine(line))
    .map((line) => line.replace(/^>\s?/, "").trim())
    .filter(Boolean)
    .join(" ");
  return sanitizeSpeechText(text);
}

export function sanitizeSpeechText(value: string, limit = 420) {
  const withoutUrls = value.replace(/https?:\/\/\S+/gi, " ");
  const withoutMarkdown = withoutUrls
    .replace(/!\[[^\]]*]\([^)]+\)/g, " ")
    .replace(/\[([^\]]+)]\([^)]+\)/g, "$1")
    .replace(/[*_~#>|]+/g, " ");
  const withoutEmoji = Array.from(withoutMarkdown)
    .filter((ch) => {
      const code = ch.codePointAt(0) ?? 0;
      return !(
        (code >= 0x1F000 && code <= 0x1FAFF) ||
        (code >= 0x2600 && code <= 0x27BF) ||
        code === 0xFE0F
      );
    })
    .join("");
  const punctuationMap: Record<string, string> = {
    "，": "，",
    "、": "，",
    "；": "；",
    "：": "："
  };
  const cleaned = withoutEmoji
    .replace(/\[[^\]]{0,32}(?:表情|图片|文件|附件|语音|动作|media|emoji)[^\]]{0,32}\]/gi, " ")
    .replace(/[“”]/g, "\"")
    .replace(/[‘’]/g, "'")
    .replace(/[（]/g, "(")
    .replace(/[）]/g, ")")
    .replace(/[，、；：]/g, (match) => punctuationMap[match] ?? match)
    .replace(/[!?！？。,.，、；;：:]{3,}/g, (match) => match[0])
    .replace(/\s+/g, " ")
    .trim();
  if (cleaned.length <= limit) return cleaned;
  const clipped = cleaned.slice(0, limit);
  const sentenceEnd = Math.max(
    clipped.lastIndexOf("。"),
    clipped.lastIndexOf("！"),
    clipped.lastIndexOf("？"),
    clipped.lastIndexOf("."),
    clipped.lastIndexOf("!"),
    clipped.lastIndexOf("?")
  );
  return (sentenceEnd > 80 ? clipped.slice(0, sentenceEnd + 1) : clipped).trim();
}
