import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { makeInput, InputError } from "../../ui/config.mjs";
import { buildCatalog } from "../../ui/presets.mjs";
const catalog = buildCatalog(
  JSON.parse(
    await readFile(new URL("../../ui/radio-presets.json", import.meta.url)),
  ),
);
const channels = catalog.find((p) => p.id === "east").channels;
const values = {
  kind: "rtl_tcp",
  host: "127.0.0.1",
  port: "1234",
  rate: "240000",
  center: "352.603125",
  gain: "",
  ppm: "0",
  path: "",
  args: "",
  antenna: "",
  "rx-channel": "0",
};
test("all 22 frequencies fit the requested RTL-TCP capture", () => {
  const input = makeInput(values, channels);
  assert.deepEqual(input, {
    kind: "rtl_tcp",
    radio: {
      host: "127.0.0.1",
      port: 1234,
      sample_rate: 240000,
      center_hz: 352603125,
      gain_db: null,
      ppm: 0,
    },
  });
});
test("field validation identifies the recoverable control before connecting", () => {
  for (const [field, value] of [
    ["host", "https://example.com"],
    ["host", ""],
    ["port", ""],
    ["port", "0"],
    ["port", "3.5"],
    ["rate", "500000"],
    ["center", "351"],
    ["center", ""],
    ["ppm", ""],
    ["gain", "-11"],
  ]) {
    assert.throws(
      () => makeInput({ ...values, [field]: value }, channels),
      (e) => e instanceof InputError && e.field === field,
    );
  }
  assert.throws(
    () => makeInput(values, []),
    (e) => e.field === "all",
  );
});
test("wave and Soapy inputs keep their own settings", () => {
  assert.deepEqual(
    makeInput(
      { ...values, kind: "wave", path: "/tmp/a.wav", rate: "" },
      channels,
    ),
    { kind: "wave", path: "/tmp/a.wav", center_hz: 352603125 },
  );
  const sdr = makeInput(
    { ...values, kind: "sdr", args: "driver=rtlsdr", gain: "19.7" },
    channels,
  );
  assert.equal(sdr.radio.gain_db, 19.7);
  assert.equal(sdr.radio.channel, 0);
  assert.throws(
    () => makeInput({ ...values, kind: "sdr" }, channels),
    (e) => e.field === "args",
  );
});
test("IPv6 and edge rates are accepted consistently with the backend", () => {
  assert.equal(
    makeInput({ ...values, host: "::1" }, channels).radio.host,
    "::1",
  );
  for (const rate of [225001, 300000, 900001, 3200000])
    assert.equal(
      makeInput({ ...values, rate: String(rate) }, channels).radio.sample_rate,
      rate,
    );
});
test("all bundled profiles fit RTL-TCP at their recommended centers and rates", () => {
  for (const preset of catalog) {
    const input = makeInput(
      {
        ...values,
        center: String(preset.center_hz / 1e6),
        rate: String(preset.sample_rate),
      },
      preset.channels,
    );
    assert.equal(input.radio.center_hz, preset.center_hz);
  }
});
test("the built-in USB driver gets rate, center, gain and correction only", () => {
  assert.deepEqual(makeInput({ ...values, kind: "usb", gain: "20", ppm: "-3" }, channels), {
    kind: "usb",
    radio: { sample_rate: 240000, center_hz: 352603125, gain_db: 20, ppm: -3 },
  });
  for (const [field, patch] of [
    ["rate", { rate: "320000" }],
    ["gain", { gain: "60" }],
    ["gain", { gain: "-1" }],
    ["ppm", { ppm: "500" }],
  ]) {
    assert.throws(
      () => makeInput({ ...values, kind: "usb", ...patch }, channels),
      (e) => e instanceof InputError && e.field === field,
      field,
    );
  }
});
