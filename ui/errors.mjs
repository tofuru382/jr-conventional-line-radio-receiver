// Short, plain error messages for the screen. The receiver's own messages
// carry technical detail (OS error codes, USB transfer errors) that helps a
// bug report but not the person holding the phone.
const RULES = [
  [/RTL-TCP: .*接続できません/, "ドライバーに接続できません。ドライバーを起動してください。"],
  [/RTL-TCP: .*(届きません|切れ|reset|closed)/i, "ドライバーとの接続が切れました。"],
  [/USB: 30 秒待ちましたが/, "ドングルを使えませんでした。挿し直して、USB の使用を許可してください。"],
  [/ドングルが戻りませんでした/, "ドングルが外れました。挿し直してから受信を開始してください。"],
  [/USB: ドングルを初期化できません/, "ドングルを初期化できません。挿し直してください。"],
  [/USB: (ドングルとの通信|ドングルから 5 秒間)/, "ドングルとの接続が切れました。"],
  [/USB: 受信を開始できません/, "ドングルの受信を開始できません。挿し直してください。"],
  [/worker (panicked|stopped)/i, "内部エラーで受信が止まりました。受信を開始し直してください。"],
  [/^Select 1–64 channels/, "受信する波を 1〜64 波選んでください。"],
];

/** Removes parenthesised parts that contain Latin letters (error codes). */
function withoutCodes(text) {
  let previous;
  do {
    previous = text;
    text = text
      .replace(/\s*\([^()]*[A-Za-z][^()]*\)/g, "")
      .replace(/\s*（[^（）]*[A-Za-z][^（）]*）/g, "");
  } while (text !== previous);
  return text.trim();
}

export function friendlyError(error) {
  const text = String(error?.message ?? error ?? "").trim();
  for (const [pattern, message] of RULES) if (pattern.test(text)) return message;
  return withoutCodes(text) || "エラーが起きました。";
}
