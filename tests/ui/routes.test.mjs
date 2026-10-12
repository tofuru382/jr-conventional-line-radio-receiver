import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { buildCatalog } from "../../ui/presets.mjs";
import { resolveRouteSelection } from "../../ui/route-picker.mjs";

const config = JSON.parse(await readFile(new URL("../../ui/radio-presets.json", import.meta.url)));
const [east, central, west] = buildCatalog(config);
const frequencies = (selection) => selection.channels.map((c) => c.frequency_hz);

test("special assignments use exact frequencies instead of the nominal D group", () => {
  const route = east.routes.find((r) => r.line === "中央本線" && r.section === "甲府～小淵沢");
  assert.equal(route.designation, "D6");
  assert.deepEqual(frequencies(resolveRouteSelection(east, [route.id], false)), [352581250, 352612500, 352662500]);
  assert.deepEqual(frequencies(resolveRouteSelection(east, [route.id])), [352581250, 352612500, 352625000, 352662500]);
});

test("multiple routes deduplicate shared frequencies and keep exceptional ones", () => {
  const normal = east.routes.find((r) => r.line === "京浜東北・根岸線");
  const special = east.routes.find((r) => r.line === "中央本線" && r.section === "甲府～小淵沢");
  const result = resolveRouteSelection(east, [normal.id, special.id, normal.id]);
  assert.equal(result.routes.length, 2);
  assert.deepEqual(frequencies(result), [352568750, 352581250, 352612500, 352625000, 352662500]);
  const same = central.routes.map((r) => r.id);
  assert.deepEqual(frequencies(resolveRouteSelection(central, same)), [352600000, 352637500, 352650000]);
});

test("all published routes fit their company profiles, with no phantom selection", () => {
  assert.deepEqual([east, central, west].map((p) => p.routes.length), [112, 2, 19]);
  for (const p of [east, central, west]) {
    for (const r of p.routes) {
      assert.equal(resolveRouteSelection(p, [r.id], false).channels.length, 3);
    }
    assert.equal(resolveRouteSelection(p, []).channels.length, 0);
    assert.throws(() => resolveRouteSelection(p, ["missing"]));
  }
  const partial = structuredClone(config);
  partial.profiles[0].services = ["common"];
  assert.equal(buildCatalog(partial)[0].routes.length, 0);
  for (const set of partial.channel_sets) {
    delete set.routes;
  }
  assert.deepEqual(buildCatalog(partial)[0].routes, []);
});
