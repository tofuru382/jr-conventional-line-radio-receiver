export class InputError extends Error {
  constructor(field, message) {
    super(message);
    this.field = field;
  }
}
export function makeInput(values, channels) {
  const bad = (field, message) => {
    throw new InputError(field, message);
  };
  const number = (id, min, max, integer = false) => {
    const n = Number(values[id]);
    if (
      String(values[id]).trim() === "" ||
      !Number.isFinite(n) ||
      n < min ||
      n > max ||
      (integer && !Number.isInteger(n))
    )
      bad(
        id,
        `${min.toLocaleString()}–${max.toLocaleString()} の${integer ? "整数" : "数値"}を入力してください。`,
      );
    return n;
  };
  if (!channels.length)
    bad("all", "受信するチャンネルを 1 波以上選んでください。");
  const center_hz = Math.round(number("center", 0.000001, 4294.967295) * 1e6);
  if (values.kind === "wave") {
    if (!values.path.trim()) bad("path", "IQ WAV ファイルを選択してください。");
    return { kind: "wave", path: values.path.trim(), center_hz };
  }
  const sample_rate = number("rate", 9600, 20000000, true);
  if (
    channels.some(
      (c) => Math.abs(c.frequency_hz - center_hz) + 4200 >= sample_rate / 2,
    )
  )
    bad(
      "center",
      `選択した ${channels.length} 波が帯域に収まるよう、中心周波数とサンプルレートを設定してください。周波数の中点は ${((Math.min(...channels.map((c) => c.frequency_hz)) + Math.max(...channels.map((c) => c.frequency_hz))) / 2e6).toFixed(6)} MHz です。`,
    );
  const gain_db = values.gain === "" ? null : number("gain", -100, 100);
  if (values.kind === "usb") {
    if (
      !(sample_rate > 225000 && sample_rate <= 300000) &&
      !(sample_rate > 900000 && sample_rate <= 3200000)
    )
      bad(
        "rate",
        "USB 直結は 225001–300000 または 900001–3200000 S/s に対応します。",
      );
    if (gain_db !== null && (gain_db < 0 || gain_db > 52.5))
      bad("gain", "USB 直結のゲインは 0–52.5 dB を指定するか、空欄で自動にしてください。");
    return {
      kind: "usb",
      radio: { sample_rate, center_hz, gain_db, ppm: number("ppm", -488, 488, true) },
    };
  }
  if (values.kind === "rtl_tcp") {
    const host = values.host.trim();
    let validIp =
      /^(\d{1,3}\.){3}\d{1,3}$/.test(host) &&
      host.split(".").every((n) => Number(n) <= 255 && String(Number(n)) === n);
    if (host.includes(":")) {
      try {
        validIp = new URL(`http://[${host}]/`).hostname.includes(":");
      } catch {}
    }
    if (!validIp)
      bad(
        "host",
        "利用する RTL-TCP サーバーの IPv4 / IPv6 アドレスを入力してください。",
      );
    const port = number("port", 1, 65535, true),
      ppm = number("ppm", -1000, 1000, true);
    if (
      !(sample_rate > 225000 && sample_rate <= 300000) &&
      !(sample_rate > 900000 && sample_rate <= 3200000)
    )
      bad(
        "rate",
        "RTL-TCP は 225001–300000 または 900001–3200000 S/s に対応します。選択したプリセットの全周波数が収まるレートを指定してください。",
      );
    if (gain_db !== null && gain_db < -10)
      bad(
        "gain",
        "RTL-TCP のゲインは −10–100 dB を指定するか、空欄で自動にしてください。",
      );
    return {
      kind: "rtl_tcp",
      radio: {
        host: values.host.trim(),
        port,
        sample_rate,
        center_hz,
        gain_db,
        ppm,
      },
    };
  }
  if (!values.args.trim())
    bad("args", "SDR を検索して選択するか、SoapySDR 引数を入力してください。");
  return {
    kind: "sdr",
    radio: {
      args: values.args.trim(),
      sample_rate,
      center_hz,
      gain_db,
      antenna: values.antenna,
      channel: number("rx-channel", 0, 255, true),
    },
  };
}
