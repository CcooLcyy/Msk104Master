import assert from "node:assert/strict";
import { test, type TestContext } from "node:test";
import { setImmediate } from "node:timers/promises";
import { createUpdateController, type UpdateInfo, type UpdateViewState } from "../src/updateController.ts";

const candidate: UpdateInfo = { version: "0.1.0-ci.99", body: "更新说明" };

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: Error) => void;
  const promise = new Promise<T>((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}

function setup(t: TestContext, check = async (): Promise<UpdateInfo | null> => null,
  install = async (): Promise<void> => {}) {
  // 中文日志通过诊断事件输出，规避 Node 22 测试运行器的 stdout 反序列化缺陷。
  t.mock.method(console, "info", (message: unknown) => t.diagnostic(String(message)));
  t.mock.method(console, "warn", (message: unknown) => t.diagnostic(String(message)));
  t.mock.timers.enable({ apis: ["setTimeout", "setInterval"] });
  const commands = { check: t.mock.fn(check), install: t.mock.fn(install) };
  const changes: UpdateViewState[] = [];
  const controller = createUpdateController(commands, (state) => changes.push(state));
  const stop = controller.start();
  t.after(stop);
  return { controller, stop, commands, changes };
}

async function settle() {
  // 等待检查与自动安装的 Promise 链结束，不推进模拟时钟。
  await setImmediate();
}

// 启动和每分钟检查发现新版本后直接安装，无需用户点击。
test("启动及一分钟周期发现更新后立即安装", async (t) => {
  const { commands, changes } = setup(t, async () => candidate);
  t.mock.timers.tick(0);
  await settle();
  assert.equal(commands.check.mock.callCount(), 1);
  assert.equal(commands.install.mock.callCount(), 1);
  assert.equal(changes.at(-1)?.candidate, null);
  assert.equal(changes.at(-1)?.busy, null);
  t.mock.timers.tick(59_999);
  assert.equal(commands.check.mock.callCount(), 1);
  assert.equal(commands.install.mock.callCount(), 1);
  t.mock.timers.tick(1);
  await settle();
  assert.equal(commands.check.mock.callCount(), 2);
  assert.equal(commands.install.mock.callCount(), 2);
});

// 没有新版本时只检查，不调用安装命令。
test("启动及周期检查无新版本时不安装", async (t) => {
  const { commands, changes } = setup(t);
  t.mock.timers.tick(0);
  await settle();
  t.mock.timers.tick(60_000);
  await settle();
  assert.equal(commands.check.mock.callCount(), 2);
  assert.equal(commands.install.mock.callCount(), 0);
  assert.equal(changes.at(-1)?.candidate, null);
  assert.equal(changes.at(-1)?.failed, false);
});

// 手动检查与自动检查共用发现新版本即安装的行为。
test("手动检查发现更新后直接安装", async (t) => {
  const { controller, commands, changes } = setup(t, async () => candidate);
  await controller.check();
  assert.equal(commands.check.mock.callCount(), 1);
  assert.equal(commands.install.mock.callCount(), 1);
  assert.equal(changes.at(-1)?.candidate, null);
  assert.equal(changes.at(-1)?.busy, null);
});

// 同一事件循环内手动、启动和周期检查共用同步锁，不积压检查请求。
test("检查未完成时跳过自动与手动检查", async (t) => {
  const request = deferred<UpdateInfo | null>();
  const { controller, commands } = setup(t, () => request.promise);
  const checking = controller.check();
  t.mock.timers.tick(0);
  t.mock.timers.tick(60_000);
  await controller.check();
  assert.equal(commands.check.mock.callCount(), 1);
  request.resolve(null);
  await checking;
  t.mock.timers.tick(60_000);
  await settle();
  assert.equal(commands.check.mock.callCount(), 2);
});

// 自动安装需要已检查候选；安装期间拒绝重复安装并跳过所有检查。
test("自动安装期间暂停检查，完成后恢复周期", async (t) => {
  const download = deferred<void>();
  const { controller, commands, changes } = setup(t, async () => candidate, () => download.promise);
  await controller.install();
  assert.equal(commands.install.mock.callCount(), 0);
  const checking = controller.check();
  await settle();
  assert.equal(commands.install.mock.callCount(), 1);
  assert.equal(changes.at(-1)?.busy, "installing");
  await controller.install();
  await controller.check();
  t.mock.timers.tick(0);
  t.mock.timers.tick(60_000);
  assert.equal(commands.install.mock.callCount(), 1);
  assert.equal(commands.check.mock.callCount(), 1);
  download.resolve();
  await checking;
  t.mock.timers.tick(60_000);
  await settle();
  assert.equal(commands.check.mock.callCount(), 2);
  assert.equal(commands.install.mock.callCount(), 2);
});

// 检查失败清空旧候选、显示错误，下个周期仍能重试成功。
test("网络错误后下一周期恢复检查", async (t) => {
  let attempts = 0;
  const { controller, commands, changes } = setup(t, async () => {
    attempts++;
    if (attempts === 2) throw new Error("网络不可用");
    return candidate;
  });
  t.mock.timers.tick(0);
  await settle();
  await controller.check();
  assert.equal(changes.at(-1)?.candidate, null);
  assert.equal(changes.at(-1)?.failed, true);
  assert.match(changes.at(-1)?.message ?? "", /网络不可用/);
  assert.equal(commands.install.mock.callCount(), 1);
  t.mock.timers.tick(60_000);
  await settle();
  assert.equal(attempts, 3);
  assert.equal(commands.install.mock.callCount(), 2);
  assert.equal(changes.at(-1)?.candidate, null);
  assert.equal(changes.at(-1)?.failed, false);
});

// 卸载取消计时器，进行中检查发现更新也不得安装或通知已卸载界面。
test("卸载后停止检查且不安装迟到的新版本", async (t) => {
  const request = deferred<UpdateInfo | null>();
  const { stop, commands, changes } = setup(t, () => request.promise);
  t.mock.timers.tick(0);
  stop();
  const notifications = changes.length;
  request.resolve(candidate);
  await settle();
  t.mock.timers.tick(180_000);
  assert.equal(commands.check.mock.callCount(), 1);
  assert.equal(commands.install.mock.callCount(), 0);
  assert.equal(changes.length, notifications);
});

// React StrictMode 立即清理并重建 effect 时，首次检查只执行一次。
test("立即重建订阅不会重复启动检查", async (t) => {
  const { stop, controller, commands } = setup(t, async () => candidate);
  stop();
  const stopAgain = controller.start();
  t.after(stopAgain);
  t.mock.timers.tick(0);
  await settle();
  assert.equal(commands.check.mock.callCount(), 1);
  assert.equal(commands.install.mock.callCount(), 1);
  stopAgain();
  t.mock.timers.tick(60_000);
  assert.equal(commands.check.mock.callCount(), 1);
  assert.equal(commands.install.mock.callCount(), 1);
});

// 安装失败保留候选并释放锁，用户可再次点击安装。
test("安装失败后仍可手动重试", async (t) => {
  let attempts = 0;
  const { controller, commands, changes } = setup(t, async () => candidate, async () => {
    if (++attempts === 1) throw new Error("下载失败");
  });
  t.mock.timers.tick(0);
  await settle();
  assert.equal(commands.install.mock.callCount(), 1);
  assert.equal(changes.at(-1)?.failed, true);
  assert.match(changes.at(-1)?.message ?? "", /下载失败/);
  assert.deepEqual(changes.at(-1)?.candidate, candidate);
  await controller.install();
  assert.equal(commands.install.mock.callCount(), 2);
  assert.equal(changes.at(-1)?.failed, false);
});

// 安装失败不阻止下个周期重新检查并自动安装，不需要用户重试。
test("自动安装失败后下一周期自动重试", async (t) => {
  let attempts = 0;
  const { commands, changes } = setup(t, async () => candidate, async () => {
    if (++attempts === 1) throw new Error("下载失败");
  });
  t.mock.timers.tick(0);
  await settle();
  assert.equal(commands.install.mock.callCount(), 1);
  assert.deepEqual(changes.at(-1)?.candidate, candidate);
  assert.equal(changes.at(-1)?.busy, null);
  assert.equal(changes.at(-1)?.failed, true);
  t.mock.timers.tick(60_000);
  await settle();
  assert.equal(commands.check.mock.callCount(), 2);
  assert.equal(commands.install.mock.callCount(), 2);
  assert.equal(changes.at(-1)?.candidate, null);
  assert.equal(changes.at(-1)?.failed, false);
});
