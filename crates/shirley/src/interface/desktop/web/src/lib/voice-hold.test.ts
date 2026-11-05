import assert from "node:assert/strict";
import test from "node:test";
import {
  HOLD_LIFT_PX,
  HOLD_TAP_MS,
  HOLD_TAP_SLOP_PX,
  HOLD_TARGET_SLOP_PX,
  composeUtterance,
  liftLineOf,
  pressIntent,
  zoneAt,
} from "./voice-hold.ts";

// The bar sits at the bottom of the screen; the finger comes down at y = 800.
const ORIGIN_Y = 800;
const LIFT_LINE = liftLineOf(ORIGIN_Y);
// Two small targets near the two sides of the screen, above the line.
const CANCEL = { left: 24, top: 120, right: 120, bottom: 160 };
const EDIT = { left: 260, top: 120, right: 356, bottom: 160 };

test("手指还停在输入条高度：松手就是发送", () => {
  assert.equal(zoneAt({ x: 200, y: ORIGIN_Y, liftLine: LIFT_LINE, cancel: CANCEL, edit: EDIT }), "send");
  assert.equal(zoneAt({ x: 200, y: LIFT_LINE + 20, liftLine: LIFT_LINE, cancel: CANCEL, edit: EDIT }), "send");
});

test("线正好压在触点上仍然算发送（边界属于不取消）", () => {
  assert.equal(zoneAt({ x: 200, y: LIFT_LINE, liftLine: LIFT_LINE, cancel: CANCEL, edit: EDIT }), "send");
});

test("抬过线就是取消，不用瞄准 —— 这是拇指盖住屏幕时唯一好做的动作", () => {
  assert.equal(zoneAt({ x: 200, y: LIFT_LINE - 1, liftLine: LIFT_LINE, cancel: CANCEL, edit: EDIT }), "cancel");
  assert.equal(zoneAt({ x: 200, y: 300, liftLine: LIFT_LINE, cancel: CANCEL, edit: EDIT }), "cancel");
});

test("落在编辑按钮上 = 编辑", () => {
  assert.equal(zoneAt({ x: 308, y: 140, liftLine: LIFT_LINE, cancel: CANCEL, edit: EDIT }), "edit");
});

test("指尖盖住按钮、触点偏出一点点，仍然算编辑", () => {
  // 斜角上偏出去一段，直线距离仍在容差内（两条轴各偏 10px ≈ 14px）。
  const inside = Math.floor((HOLD_TARGET_SLOP_PX - 2) / Math.SQRT2) + 2;
  assert.equal(
    zoneAt({ x: EDIT.right + inside, y: EDIT.bottom + inside, liftLine: LIFT_LINE, cancel: CANCEL, edit: EDIT }),
    "edit",
  );
});

test("贴在取消按钮上也是取消，抬起来本来就是取消", () => {
  assert.equal(zoneAt({ x: 72, y: 140, liftLine: LIFT_LINE, cancel: CANCEL, edit: EDIT }), "cancel");
});

test("目标还没量出来时：抬起来仍然能取消，没抬就是发送", () => {
  const bare = { liftLine: LIFT_LINE, cancel: null, edit: null };
  assert.equal(zoneAt({ x: 200, y: 300, ...bare }), "cancel");
  assert.equal(zoneAt({ x: 200, y: ORIGIN_Y, ...bare }), "send");
});

test("抬起的距离就是那一条线，不随目标位置变化", () => {
  assert.equal(LIFT_LINE, ORIGIN_Y - HOLD_LIFT_PX);
});

test("识别器只给出 settled 时交付 settled", () => {
  assert.equal(composeUtterance("先这样", ""), "先这样");
});

test("只有 interim 也不能丢 —— 「松手无法发送」的回归测试", () => {
  assert.equal(composeUtterance("", "还在变的那半句"), "还在变的那半句");
});

test("settled 与 interim 按顺序拼接，并清理首尾空白", () => {
  assert.equal(composeUtterance("  前半句 ", " 后半句  "), "前半句 后半句");
});

test("什么都没听到时是空串，调用方据此静默丢弃而不是发一条空消息", () => {
  assert.equal(composeUtterance("", "  ", ""), "");
});

// --- 同一个输入条：轻点是打字，按住是说话 -------------------------------------

test("轻点：手指只停了一下，是要打字，不是要录音", () => {
  assert.equal(pressIntent(90, 0), "tap");
});

test("按住：超过阈值才开始录音", () => {
  assert.equal(pressIntent(260, 0), "hold");
});

test("恰好到阈值就算按住，边界不留给实现猜", () => {
  assert.equal(pressIntent(HOLD_TAP_MS, 0), "hold");
  assert.equal(pressIntent(HOLD_TAP_MS - 1, 0), "tap");
});

test("容差以内的抖动仍是一次轻点（拇指从不完全静止）", () => {
  assert.equal(pressIntent(120, HOLD_TAP_SLOP_PX), "tap");
});

test("手指滑走了：既不开麦克风，也不弹键盘", () => {
  assert.equal(pressIntent(60, HOLD_TAP_SLOP_PX + 1), "abandoned");
  assert.equal(pressIntent(900, 80), "abandoned");
});
