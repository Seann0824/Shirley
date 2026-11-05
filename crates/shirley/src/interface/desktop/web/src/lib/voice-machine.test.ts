import assert from "node:assert/strict";
import test from "node:test";
import {
  HEARD_NOTHING,
  NO_DICTATION,
  TRANSCRIBE_STALLED,
  idleVoice,
  voiceReduce,
  type TranscriberKind,
  type VoiceDelivery,
  type VoiceEffect,
  type VoiceEvent,
} from "./voice-machine.ts";

/**
 * One dictation is a sequence, so the tests are sequences. `Session` drives the reducer the way
 * the hook does and remembers what it was asked to do; the generation numbers come from the
 * machine itself (`opened()` means "the live generation opened"), so a test can only be wrong
 * about behaviour, never about arithmetic. Stale events are injected on purpose, by hand.
 */
class Session {
  state = idleVoice();
  effects: VoiceEffect[] = [];
  readonly engine: TranscriberKind | null;

  constructor(engine: TranscriberKind | null = "web") {
    this.engine = engine;
  }

  send(event: VoiceEvent): this {
    const step = voiceReduce(this.state, event, { engine: this.engine });
    this.state = step.state;
    this.effects.push(...step.effects);
    return this;
  }

  press(delivery: VoiceDelivery = "send"): this {
    return this.send({ type: "press", delivery });
  }
  release(delivery: VoiceDelivery = "send"): this {
    return this.send({ type: "release", delivery });
  }
  /** The microphone for the *live* generation opened. */
  opened(): this {
    return this.send({ type: "opened", gen: this.state.gen });
  }
  heard(settled: string, interim = ""): this {
    return this.send({ type: "heard", gen: this.state.gen, settled, interim });
  }
  settled(text: string): this {
    return this.send({ type: "settled", gen: this.state.gen, text });
  }
  failed(message: string): this {
    return this.send({ type: "failed", gen: this.state.gen, message });
  }
  cancel(): this {
    return this.send({ type: "cancel" });
  }
  expired(): this {
    return this.send({ type: "expired", gen: this.state.gen });
  }

  get delivered() {
    return this.effects.filter(
      (effect): effect is Extract<VoiceEffect, { type: "deliver" }> => effect.type === "deliver",
    );
  }
  get kinds() {
    return this.effects.map((effect) => effect.type);
  }
}

test("按住说话、松手发送：交付听到的整句", () => {
  const s = new Session().press("send").opened().heard("帮我看看", "邮件").release("send");
  assert.deepEqual(s.delivered, [{ type: "deliver", text: "帮我看看 邮件", delivery: "send" }]);
  assert.equal(s.state.phase, "idle");
});

/**
 * The failure the user reported, at the level of the machine.
 *
 * iOS answers push-to-talk with *interim* results only: the final list we used to wait for came
 * back empty, so a sentence that was on screen the whole time was retired as `HEARD_NOTHING`.
 */
test("只有临时结果也照样交付：等在屏幕上的字就是用户说的话", () => {
  const s = new Session().press("send").opened().heard("", "明天提醒我交电费").release("send");
  assert.deepEqual(s.delivered, [
    { type: "deliver", text: "明天提醒我交电费", delivery: "send" },
  ]);
  assert.equal(s.state.error, "", "听到了字就不许说没听清");
});

/**
 * And the wait:松手就是交付，不是「等识别器哪天回一个 end」。
 * Nothing here may ask for a graceful stop -- that request is the seconds the user spent waiting.
 */
test("松手立刻交付并硬关会话，不等识别器回 end", () => {
  const s = new Session().press("send").opened().heard("", "第一句").release("send");
  assert.deepEqual(s.kinds, ["open", "abort", "deliver"], "先收会话，再交付，没有 stop");
});

test("松手发生在麦克风开完之前：下一段照样能按住，迟到的开麦不许复活", () => {
  // 第一次：按下、立刻松手，麦克风还在权限往返里。手上没有一个字，不该交付任何东西。
  const s = new Session().press("send").release("send");
  assert.deepEqual(s.delivered, []);
  assert.equal(s.state.phase, "idle");

  // 那次开麦之后才成功 -- 它属于已经作废的那一代，机器不许被它带回 listening。
  const late = voiceReduce(s.state, { type: "opened", gen: 1 }, { engine: "web" });
  assert.equal(late.state.phase, "idle");
  assert.deepEqual(late.effects, []);

  // 于是第二次按住必须真的开麦、真的交付。
  const next = new Session().press("send").opened().heard("第二句").release("send");
  assert.deepEqual(next.delivered, [{ type: "deliver", text: "第二句", delivery: "send" }]);
  assert.equal(next.state.phase, "idle");
});

test("开麦失败后按下一次，不会被旧状态拦住", () => {
  const s = new Session().press("send").failed("没有麦克风权限，所以听不到你说话。");
  assert.equal(s.state.phase, "idle");
  assert.equal(s.state.error, "没有麦克风权限，所以听不到你说话。");

  s.press("send");
  assert.equal(s.state.phase, "opening");
  assert.equal(s.state.error, "", "新的一段不该继承上一段的错误");
});

test("上一代迟到的文字不写进这一代", () => {
  const s = new Session().press("send").opened().release("send").press("send").opened();
  s.send({ type: "heard", gen: 1, settled: "上一代的句子", interim: "" });
  s.heard("这一代的句子").release("send");
  assert.deepEqual(s.delivered, [{ type: "deliver", text: "这一代的句子", delivery: "send" }]);
  assert.equal(s.state.phase, "idle");
});

test("说话中又按一下 = 重新开一段，旧的被丢掉", () => {
  const s = new Session().press("send").opened().heard("旧句子");
  const before = s.effects.length;
  s.press("send");
  assert.equal(s.state.phase, "opening");
  assert.equal(s.state.settled, "");
  assert.deepEqual(s.kinds.slice(before), ["abort", "open"], "先收掉旧的一段，再开新的");
});

test("本地引擎：松手先停录转写，拿到文字才交付", () => {
  const s = new Session("local").press("send").opened().release("send");
  assert.equal(s.state.phase, "finishing");
  assert.deepEqual(s.kinds, ["open", "close"]);
  assert.deepEqual(s.delivered, [], "文字还没出来，不能交付");

  s.settled(" 转写结果 ");
  assert.equal(s.state.phase, "idle");
  assert.deepEqual(s.delivered, [{ type: "deliver", text: "转写结果", delivery: "send" }]);
});

test("本地引擎转写期间又按下：在途的那段作废，迟到的结果不交付", () => {
  const s = new Session("local").press("send").opened().release("send");
  s.press("send");
  assert.equal(s.state.phase, "opening");
  assert.deepEqual(s.kinds.slice(-2), ["abort", "open"]);

  const before = s.delivered.length;
  s.send({ type: "settled", gen: 1, text: "上一段的结果" });
  assert.equal(s.delivered.length, before, "旧结果不许交付");
  assert.equal(s.state.phase, "opening", "新的一段还在开麦，不能被旧结果带回 idle");
});

test("取消永远不交付，而且收掉正在用的那一段", () => {
  const s = new Session().press("send").opened().heard("别发出去").cancel();
  assert.deepEqual(s.delivered, []);
  assert.equal(s.state.phase, "idle");
  assert.deepEqual(s.effects.at(-1), { type: "abort", gen: 1 });
});

test("什么都没听到就松手：不产生空的一轮，但要说一句", () => {
  // 按着说了话、麦克风也亮了，识别器却一个字都没给——沉默是这个手势里唯一会让人
  // 以为「app 坏了」的结局，所以它按失败处理，给一句能照做的话。
  const s = new Session().press("send").opened().release("send");
  assert.deepEqual(s.delivered, [], "不产生空的一轮");
  assert.equal(s.state.phase, "idle");
  assert.equal(s.state.error, HEARD_NOTHING);
  assert.deepEqual(s.effects.at(-1), { type: "abort", gen: 1 }, "识别器照样要收掉");
});

test("听到了字就不再多话", () => {
  const s = new Session().press("send").opened().heard("你好").release("send");
  assert.equal(s.state.error, "");
});

test("60 秒上限等于把手指抬起来：交付已经听到的部分", () => {
  const s = new Session().press("send").opened().heard("说了一半").expired();
  assert.deepEqual(s.delivered, [{ type: "deliver", text: "说了一半", delivery: "send" }]);
  assert.equal(s.state.phase, "idle");
});

test("本地转写卡住时不能永远停在 finishing", () => {
  // The finger is up, the capture is closed and whisper never answers. Before this, `expired`
  // was dropped in `finishing` and the machine stayed there for good: no microphone, no message.
  const s = new Session("local").press("send").opened().release("send").expired();
  assert.equal(s.state.phase, "idle");
  assert.equal(s.state.error, TRANSCRIBE_STALLED);
  assert.deepEqual(s.kinds, ["open", "close", "abort"], "放弃这一句，会话也要收掉");
});

test("编辑模式跟着交付一起走", () => {
  const s = new Session().press("edit").opened().heard("先改一下").release("edit");
  assert.deepEqual(s.delivered, [{ type: "deliver", text: "先改一下", delivery: "edit" }]);
});

test("没有可用转写时不假装在听", () => {
  const s = new Session(null).press("send");
  assert.equal(s.state.phase, "idle");
  assert.deepEqual(s.effects, [], "不开麦克风");
  assert.equal(s.state.error, NO_DICTATION);
});

test("旧世代的失败不会把新的一段关掉", () => {
  const s = new Session().press("send").opened().release("send").press("send").opened();
  s.send({ type: "failed", gen: 1, message: "旧的失败" });
  assert.equal(s.state.phase, "listening");
  assert.equal(s.state.error, "");
});
