import { test } from "node:test";
import assert from "node:assert/strict";
import { AudioPlayer, Leveler } from "../../ui/audio.mjs";

const pcm = Array(320).fill(8000);
const frame = (index, epoch = 0, channel = "a") => ({ channel, epoch, index, pcm });
const burst = (first, count) =>
  Array.from({ length: count }, (_, i) => frame(first + i));

function setup() {
  const nodes = [];
  const context = {
    state: "running",
    currentTime: 0,
    createBuffer(_channels, length, rate) {
      const data = new Float32Array(length);
      return { duration: length / rate, getChannelData: () => data };
    },
    createBufferSource() {
      const node = {
        playbackRate: { value: 1 },
        connect() {},
        disconnect() {
          this.disconnected = true;
        },
        stop() {
          this.stopped = true;
        },
        start(at) {
          this.at = at;
          this.end = at + this.buffer.duration / this.playbackRate.value;
        },
      };
      nodes.push(node);
      return node;
    },
  };
  const player = new AudioPlayer(context, {});
  const advance = (time) => {
    context.currentTime = time;
    for (const node of player.sources) {
      if (node.end <= time) node.onended();
    }
  };
  return { player, nodes, context, advance };
}

test("late arrivals append to the playing tail without cancelling or inserting silence", () => {
  const { player, nodes, advance } = setup();
  player.enqueue(burst(0, 4), "a");
  const end = nodes.at(-1).end;
  advance(end - 0.01);
  player.enqueue(burst(4, 4), "a");
  assert.ok(nodes.every((n) => !n.stopped));
  for (let i = 1; i < nodes.length; i++)
    assert.ok(Math.abs(nodes[i].at - nodes[i - 1].end) < 1e-9);
  assert.equal(nodes[4].buffer.getChannelData(0)[0], 8000 / 32768);
});

test("duplicates and empty polls keep queued audio; only new epochs replace it", () => {
  const { player, nodes } = setup();
  player.enqueue(burst(0, 4), "a");
  player.enqueue([], "a");
  player.enqueue([frame(2), frame(3), frame(4, 0, "b")], "a");
  assert.equal(nodes.length, 4);
  assert.ok(nodes.every((n) => !n.stopped));
  player.enqueue([frame(0, 1)], "a");
  assert.ok(nodes.slice(0, 4).every((n) => n.stopped && n.disconnected));
  player.enqueue([frame(100, 0)], "a");
  assert.equal(nodes.length, 5);
  assert.equal(nodes[4].stopped, undefined);
});

test("mute/stop and channel switches discard the old channel and clean up nodes", () => {
  const { player, nodes, advance } = setup();
  player.enqueue(burst(0, 4), "a");
  player.enqueue([frame(0, 0, "b")], "b");
  assert.ok(nodes.slice(0, 4).every((n) => n.stopped && n.disconnected));
  player.reset();
  assert.equal(player.active, false);
  assert.ok(nodes.every((n) => n.stopped && n.disconnected));
  player.enqueue([frame(0)], "a");
  assert.equal(player.active, true);
  advance(nodes.at(-1).end);
  assert.equal(player.active, false);
  assert.equal(nodes.at(-1).disconnected, true);
});

test("real frame loss preserves its silence and a long idle interval starts a new buffer", () => {
  const { player, nodes, advance } = setup();
  player.enqueue([frame(0), frame(2)], "a");
  assert.ok(Math.abs(nodes[1].at - nodes[0].end - 0.04) < 1e-9);
  advance(8);
  player.enqueue([frame(200)], "a");
  assert.ok(nodes[2].at > 8 && nodes[2].at < 8.5);
  assert.ok(nodes.every((n) => !n.stopped));
  // After an actual underrun, buffer again without re-enqueuing old frames.
  advance(9);
  player.enqueue([frame(200), frame(201), frame(202)], "a");
  assert.equal(nodes.length, 5);
  assert.ok(nodes[3].at > 9);
  assert.equal(nodes[4].at, nodes[3].end);
});

test("exceptional bursts cannot build an unbounded playback queue or replay dropped frames", () => {
  const { player, nodes, advance } = setup();
  player.enqueue(burst(0, 100), "a");
  assert.ok(nodes.at(-1).end <= 1.5);
  assert.ok(player.sources.size <= 39);
  const count = nodes.length;
  const end = nodes.at(-1).end;
  advance(end - 0.1);
  player.enqueue(burst(50, 50), "a");
  assert.equal(nodes.length, count);
  player.enqueue([frame(100)], "a");
  assert.equal(nodes.at(-1).at, end);
  assert.ok(nodes.every((n) => !n.stopped));
});

for (const clockRate of [0.999, 1, 1.001]) {
  test(`half-second input bursts and ${clockRate}x input clock play without periodic gaps`, () => {
    for (const intervals of [
      [0.18],
      [0.18, 0.16, 0.24, 0.14, 0.28, 0.1, 0.18, 0.2],
    ]) {
      const { player, nodes, advance } = setup();
      let time = 0,
        index = 0,
        end,
        count = 0;
      for (let poll = 0; poll < 10000; poll++) {
        // The UI polls independently of a source delivering 560 ms at once.
        // Four frames remain in the voice decoder's synthesis lookahead.
        advance(time);
        const available = Math.max(
          0,
          Math.floor((time * clockRate) / 0.56) * 14 - 4,
        );
        player.enqueue(burst(index, available - index), "a");
        for (const node of nodes) {
          assert.ok(!node.stopped);
          assert.ok(node.at >= time, `late frame at ${time}`);
          assert.ok(node.end <= time + 1.5, `unbounded latency at ${time}`);
          if (end !== undefined)
            assert.ok(Math.abs(node.at - end) < 1e-9, `gap or overlap at ${time}`);
          end = node.end;
          count++;
        }
        nodes.length = 0;
        // Include the partly played frame and a playback rate above 1.
        assert.ok(player.sources.size <= 39);
        index = available;
        time += intervals[poll % intervals.length];
      }
      assert.equal(count, index, "no PCM frames dropped during batched delivery");
      advance(end);
      assert.equal(player.active, false);
    }
  });

  test(`30 minutes of bursty delivery and ${clockRate}x input clock stay continuous and bounded`, () => {
    const { player, nodes, advance } = setup();
    const intervals = [0.18, 0.16, 0.24, 0.14, 0.28, 0.1, 0.18, 0.2];
    let time = 0,
      index = 0,
      end,
      count = 0;
    for (let poll = 0; time < 1800; poll++) {
      advance(time);
      const available = Math.floor(((time + 0.16) * clockRate) / 0.04);
      player.enqueue(burst(index, available - index), "a");
      for (const node of nodes) {
        assert.ok(!node.stopped);
        assert.ok(node.at >= time, `late frame at ${time}`);
        if (end !== undefined)
          assert.ok(Math.abs(node.at - end) < 1e-9, `gap or overlap at ${time}`);
        assert.ok(Math.abs(node.playbackRate.value - 1) <= 0.005);
        end = node.end;
        count++;
      }
      nodes.length = 0; // Retain only live nodes, as the browser does.
      assert.ok(
        end - time > 0.1 && end - time < 1,
        `unbounded latency at ${time}: ${end - time}`,
      );
      assert.ok(player.sources.size < 25);
      index = available;
      time += intervals[poll % intervals.length];
    }
    assert.equal(count, index);
    advance(end);
    assert.equal(player.active, false);
  });
}

const toneAt = (amplitude, n) =>
  Array.from({ length: n }, (_, i) => Math.trunc(Math.sin(i * 0.3) * amplitude * 32767));
const rms = (pcm) => Math.sqrt(pcm.reduce((s, x) => s + (x / 32768) ** 2, 0) / pcm.length);

test("leveler raises quiet speech, keeps loud speech unclipped and never boosts hiss", () => {
  const quiet = new Leveler().process(toneAt(0.03, 16000));
  assert.ok(rms(quiet.slice(12000)) > 0.03 * 0.707 * 3);
  const l = new Leveler();
  const loud = l.process(toneAt(0.95, 16000));
  assert.ok(l.gain <= 1);
  assert.ok(loud.every((s) => s > -32768 && s < 32767));
  const hiss = toneAt(0.002, 16000);
  const quietLeveler = new Leveler();
  assert.deepEqual(quietLeveler.process(hiss), hiss);
  assert.equal(quietLeveler.gain, 1);
});
