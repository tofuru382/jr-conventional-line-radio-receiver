import { test } from "node:test";
import assert from "node:assert/strict";
import { friendlyError } from "../../ui/errors.mjs";

test("known receiver errors become one short sentence", () => {
  assert.equal(
    friendlyError("RTL-TCP: 127.0.0.1:1234 に接続できません。サーバーとポートを確認してください (Connection refused (os error 111))"),
    "ドライバーに接続できません。ドライバーを起動してください。",
  );
  assert.equal(
    friendlyError(
      "USB: ドングルとの通信が続けて失敗しました（USB transfer error: device disconnected）。ケーブル・ハブ・電源を確認してください（60 秒間待ちましたが、ドングルが戻りませんでした。挿し直してから受信を開始してください）",
    ),
    "ドングルが外れました。挿し直してから受信を開始してください。",
  );
  assert.equal(
    friendlyError(new Error("USB: ドングルを初期化できません (USB transfer error: device disconnected)")),
    "ドングルを初期化できません。挿し直してください。",
  );
  assert.equal(
    friendlyError("Channel worker panicked; restart reception"),
    "内部エラーで受信が止まりました。受信を開始し直してください。",
  );
});

test("other errors lose their technical codes but keep Japanese notes", () => {
  assert.equal(friendlyError("保存できません (Permission denied (os error 13))"), "保存できません");
  assert.equal(friendlyError("ゲインは 0–52.5 dB（空欄で自動）を指定してください"), "ゲインは 0–52.5 dB（空欄で自動）を指定してください");
  assert.equal(friendlyError(""), "エラーが起きました。");
});
