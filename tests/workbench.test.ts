import assert from "node:assert/strict";
import { after, before, test } from "node:test";
import { fileURLToPath } from "node:url";
import { createElement, type ComponentType } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createServer, type ViteDevServer } from "vite";

let server: ViteDevServer | undefined;
let html = "";
const invocationKey = "__workbenchTestInvocations";
const testGlobals = globalThis as typeof globalThis & { [invocationKey]?: string[] };

// 使用真实组件和现有 Vite 转换链，不引入 DOM 或浏览器依赖。
// SSR 不执行 effect；此处仅保证模块加载与初始渲染没有隐式调用命令。
before(async () => {
  testGlobals[invocationKey] = [];
  server = await createServer({
    root: fileURLToPath(new URL("..", import.meta.url)),
    server: { middlewareMode: true, hmr: false, watch: null },
    ssr: { noExternal: ["@tauri-apps/api"] },
    plugins: [{
      name: "workbench-test-tauri-boundary",
      enforce: "pre",
      resolveId(id) {
        if (id === "@tauri-apps/api/core") return "\0workbench-test-tauri";
      },
      load(id) {
        if (id === "\0workbench-test-tauri") {
          return `export function invoke(command) {
            globalThis.${invocationKey}.push(command);
            return Promise.reject(new Error("初始渲染不得调用 Tauri 命令"));
          }`;
        }
      },
    }],
  });
  const { default: App } = await server.ssrLoadModule("/src/App.tsx") as { default: ComponentType };
  html = renderToStaticMarkup(createElement(App));
});

after(async () => {
  await server?.close();
  delete testGlobals[invocationKey];
});

function text(markup: string): string {
  return markup.replace(/<[^>]*>/g, "").trim();
}

function elements(markup: string, tag: string): string[] {
  return [...markup.matchAll(new RegExp(`<${tag}\\b[^>]*>[\\s\\S]*?</${tag}>`, "g"))]
    .map((match) => match[0]);
}

function openingTag(markup: string): string {
  return markup.slice(0, markup.indexOf(">") + 1);
}

function buttonNamed(markup: string, name: string): string {
  const matches = elements(markup, "button").filter((button) => text(button) === name);
  assert.equal(matches.length, 1, `应有一个“${name}”按钮`);
  return matches[0];
}

test("顶部协议操作保持禁用，并解释尚未实现", () => {
  const header = elements(html, "header")[0];
  assert.ok(header, "工作台应有顶部操作栏");
  for (const name of ["连接", "断开", "总召", "对时"]) {
    const attributes = openingTag(buttonNamed(header, name));
    assert.match(attributes, /\sdisabled(?:=""|\s|>)/, `${name}不得发送协议命令`);
    assert.match(attributes, /\stitle="[^"]*(?:未实现|待实现)[^"]*"/);
  }
});

test("状态、通信设置、报文列表与详情在同一工作台同时存在", () => {
  const headings = elements(html, "h2").map(text);
  for (const title of ["运行状态", "通信设置", "报文监视", "报文详情"]) {
    assert.ok(headings.includes(title), `初始工作台缺少${title}`);
  }
  assert.match(text(html), /未连接/);
  assert.match(text(html), /协议.*(?:未实现|待实现)|(?:未实现|待实现).*协议/);
});

test("六类点表页签可识别，初始选择遥信", () => {
  const tabs = elements(html, "button").filter((button) => /\srole="tab"/.test(openingTag(button)));
  assert.deepEqual(tabs.map(text), ["遥信", "遥测", "遥控", "遥调", "电量", "SOE"]);
  for (const tab of tabs) {
    assert.match(openingTag(tab), new RegExp(`\\saria-selected="${text(tab) === "遥信"}"`));
    assert.match(openingTag(tab), /\saria-controls="[^"]+"/);
  }
  assert.match(html, /role="tabpanel"/);
});

test("报文及点表保持诚实空状态，不预填实际数据", () => {
  const tables = elements(html, "table");
  assert.ok(tables.length >= 2, "应同时展示报文表格与分类点表");
  for (const table of tables) {
    assert.ok(elements(table, "th").length > 0, "空表也保留列头");
    const body = elements(table, "tbody")[0];
    assert.ok(body, "表格应有表体");
    const rows = elements(body, "tr");
    assert.equal(rows.length, 1, "初始表格只有空状态行");
    const cells = elements(rows[0], "td");
    assert.equal(cells.length, 1, "不得预填点值或报文记录");
    assert.match(openingTag(cells[0]), /\scolSpan="\d+"|\scolspan="\d+"/);
    assert.match(text(cells[0]), /暂无|尚无|尚未|没有/);
  }
  assert.match(text(html), /(?:无|暂无|尚无|尚未).*报文|报文.*(?:无|暂无|尚无|尚未)/);
});

test("保留主机、端口、公共地址的必填与范围校验入口", () => {
  const labels = elements(html, "label");
  const inputFor = (name: RegExp) => {
    const label = labels.find((item) => name.test(text(item)) && /<input\b/.test(item));
    assert.ok(label, `缺少输入项 ${name}`);
    return label.match(/<input\b[^>]*>/)![0];
  };
  const host = inputFor(/主机/);
  assert.match(host, /\svalue="127\.0\.0\.1"/);
  assert.match(host, /\srequired(?:=""|\s|>)/);
  for (const [label, value, maximum] of [[/端口/, "2404", "65535"], [/公共地址|CA/, "1", "65534"]] as const) {
    const input = inputFor(label);
    for (const attribute of [`type="number"`, `min="1"`, `max="${maximum}"`, `value="${value}"`]) {
      assert.ok(input.includes(attribute), `输入项应保留 ${attribute}`);
    }
    assert.match(input, /\srequired(?:=""|\s|>)/);
  }
  assert.match(openingTag(buttonNamed(html, "校验参数")), /\stype="submit"/);
});

test("保留软件更新入口，初始渲染不隐式调用原生命令", () => {
  assert.match(html, /aria-label="软件更新"/);
  buttonNamed(html, "检查更新");
  assert.deepEqual(testGlobals[invocationKey], []);
});
