# Select receive channels by railway line

[日本語](ROUTES.ja.md)

“路線から選ぶ” (Choose by line), above the channel list, lets you choose receive targets without opening the frequency controls.

1. Choose the operator/profile. Search by line name, section endpoint, or D designation. Space-separated words narrow the search; full-width Latin characters and numbers also work.
2. Check one or more sections. Search only filters the results: selected sections remain in the preview. Use “解除” (Remove) to remove an individual section or “選択を解除” (Clear selection) to clear all sections.
3. Review the exact voice and data frequencies. Shared frequencies appear once. “旅客一斉情報も受信” adds the profile's common broadcast channel, where available; it is enabled by default. JR East's operation monitor uses this channel.
4. Apply the selection to **replace** the currently enabled receive channels. Cancel or Escape discards the draft. Applying a different profile also uses that profile's recommended tuning, except for IQ WAV input, as with the existing profile selector.
5. Start reception separately. While receiving, the dialog remains available for reference, but applying changes is disabled.

The dialog preserves manual audio selection when the selected frequency is present in the resulting profile. Choosing receive targets does not enable automatic audio selection. The existing frequency checkboxes remain available for adjustments. Reopening the dialog restores the last applied sections while their receive targets still match; drafts and receive-channel selections are not saved across application restarts.

## Data and scope

The built-in catalog transcribes the line/section tables in [odaQshirei's Posfie collection](https://posfie.com/@odaQshirei/p/EhESIhP), accessed on 2026-10-12:

| Table | Table date | Sections |
| --- | --- | ---: |
| JR East, metropolitan | 2025-01-03 | 46 |
| JR East, regional | 2026-08-28 | 66 |
| JR Central | 2025-12-21 | 2 |
| JR West | 2023-02-14 | 19 |

Frequencies follow the collection's allocation table dated 2026-08-28. The catalog stores each section's two voice frequencies and data frequency explicitly, including nonstandard voice assignments. It does not calculate frequencies from the D designation. For example, JR East's Chūō Main Line, Kōfu–Kobuchizawa, uses D6 with voice 1 at **352.61250 MHz**, voice 2 at **352.58125 MHz**, and data at **352.66250 MHz**.

Only listed sections are included. The table date is provenance, not a guarantee of current operation; the JR Central source also notes possible changes to service sections and boundaries. Selecting a line selects its frequencies and does not filter out other lines that share them. Published section boundaries do not describe radio coverage.

## External preset files

The existing version 1 JSON format accepts an optional `routes` array in each `channel_sets` entry. Files without this field remain valid and continue to support manual frequency selection. The [JSON schema](../config/radio-presets.schema.json) describes the structure. The following is metadata to add to an existing channel set, **not a standalone preset file**:

```json
{
  "routes": [
    {
      "id": "chuo-kofu-kobuchizawa",
      "line": "中央本線",
      "section": "甲府～小淵沢",
      "designation": "D6",
      "voice_hz": [352612500, 352581250],
      "data_hz": 352662500,
      "note": "変則割当：音声第1波 352.61250 MHz"
    }
  ]
}
```

- Route IDs must be unique within each channel set's `routes` array.
- `voice_hz` contains exactly two distinct integer frequencies, in voice 1 / voice 2 order, referencing channels with `service: "voice"`. `data_hz` must reference a channel with `service: "data"`. `note` is optional.
- A profile exposes a route only if its service selection includes all three frequencies. The common channel is added separately by the dialog.
- Each channel set permits up to 512 routes. Invalid route metadata rejects the entire replacement catalog and leaves the active catalog unchanged.

Reload the external file while reception is stopped to apply edits. This updates the route catalog along with the frequency definitions.
