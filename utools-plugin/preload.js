// AgentHub 助手 — uTools 插件 preload（纯 Node，零依赖）
// 后端：AgentHub CLI（agenthub.exe），本插件只做触发与展示，不含任何业务逻辑。
// exe 查找顺序：插件 bin/agenthub.exe → %LOCALAPPDATA%\AgentHub\agenthub.exe（NSIS 默认安装位）
const { spawn } = require("child_process");
const path = require("path");
const fs = require("fs");

function exePath() {
  const bundled = path.join(__dirname, "bin", "agenthub.exe");
  if (fs.existsSync(bundled)) return bundled;
  const installed = path.join(
    process.env.LOCALAPPDATA || "",
    "AgentHub",
    "agenthub.exe"
  );
  if (fs.existsSync(installed)) return installed;
  return null;
}

function runCli(args) {
  return new Promise((resolve, reject) => {
    const exe = exePath();
    if (!exe)
      return reject(
        new Error(
          "未找到 agenthub.exe（放入插件 bin/ 目录，或先安装 AgentHub 桌面版）"
        )
      );
    const p = spawn(exe, args, { windowsHide: true });
    let out = "";
    let err = "";
    p.stdout.on("data", (d) => (out += d));
    p.stderr.on("data", (d) => (err += d));
    p.on("error", reject);
    p.on("close", (code) => {
      if (code === 0) resolve(out);
      else reject(new Error(err.trim() || `agenthub 退出码 ${code}`));
    });
  });
}

function jparse(s) {
  try {
    return JSON.parse(s);
  } catch {
    return null;
  }
}

const short = (uid) => (uid || "?").slice(0, 8);

const errItem = (e) => ({
  title: "❌ " + (e.message || String(e)),
  description: "排查见 utools-plugin/README.md",
  arg: { type: "noop" },
});

let lastItems = []; // checkin 的 select 回放缓存

// ─── wb-switch：切号 ───

function renderSwitchReport(report) {
  const items = (report.steps || []).map((s) => ({
    title: "✓ " + s,
    arg: { type: "noop" },
  }));
  if (report.ok) {
    items.unshift({ title: "🎉 切号完成", arg: { type: "noop" } });
  } else {
    items.unshift({
      title: "❌ 切号失败：" + (report.error || "未知错误"),
      description: "已自动回滚（见上方步骤留痕）",
      arg: { type: "noop" },
    });
  }
  return items;
}

function doSwitch(slot, force, callbackSetList) {
  const args = ["--wb-switch", slot.id];
  if (force) args.push("--force");
  callbackSetList([{ title: "⏳ 正在切换…", arg: { type: "noop" } }]);
  runCli(args)
    .then((out) => {
      const report = jparse(out);
      if (!report) throw new Error("无法解析 --wb-switch 输出");
      callbackSetList(renderSwitchReport(report));
    })
    .catch((e) => callbackSetList([errItem(e)]));
}

window.exports = {
  "wb-switch": {
    mode: "list",
    args: {
      placeholder: "选择账号槽位（回车确认）",
      enter(action, callbackSetList) {
        callbackSetList([
          { title: "⏳ 正在读取槽位…", arg: { type: "noop" } },
        ]);
        runCli(["--wb-status"])
          .then((out) => {
            const o = jparse(out);
            if (!o || !Array.isArray(o.slots))
              throw new Error("无法解析 --wb-status 输出");
            const items = [];
            if (o.process_running)
              items.push({
                title: "⚠ WorkBuddy 正在运行",
                description: "选中槽位后可确认强制结束再切",
                arg: { type: "noop" },
              });
            for (const s of o.slots) {
              const isCurrent = o.current && o.current.uid === s.uid;
              items.push({
                title: s.label + (isCurrent ? "（当前）" : ""),
                description: `uid ${short(s.uid)} · ${s.saved_at}${
                  s.file_ok ? "" : " · ⚠槽位文件缺失"
                }`,
                arg: {
                  type: "slot",
                  id: s.id,
                  label: s.label,
                  running: !!o.process_running,
                },
              });
            }
            items.push({
              title: "💾 保存当前登录为新槽位",
              arg: { type: "save" },
            });
            callbackSetList(items);
          })
          .catch((e) => callbackSetList([errItem(e)]));
      },
      select(item, callbackSetList) {
        if (item.arg.type === "slot") {
          if (item.arg.running) {
            // WB 在跑：二次确认后才 taskkill（--force），绝不静默强杀
            callbackSetList([
              {
                title: `确认结束 WorkBuddy 并切到「${item.arg.label}」？`,
                description: "将 taskkill 结束 WB → 备份 → 恢复 → 校验 → 启动",
                arg: {
                  type: "do",
                  id: item.arg.id,
                  force: true,
                },
              },
              { title: "取消", arg: { type: "cancel" } },
            ]);
          } else {
            doSwitch(item.arg, false, callbackSetList);
          }
        } else if (item.arg.type === "do") {
          doSwitch(item.arg, item.arg.force, callbackSetList);
        } else if (item.arg.type === "save") {
          const label =
            "槽位-" + new Date().toLocaleString("sv").replace(/[ :]/g, "");
          callbackSetList([{ title: "⏳ 保存中…", arg: { type: "noop" } }]);
          runCli(["--wb-save-slot", label])
            .then(() =>
              callbackSetList([
                { title: "✓ 已保存：" + label, arg: { type: "noop" } },
              ])
            )
            .catch((e) => callbackSetList([errItem(e)]));
        } else if (item.arg.type === "cancel") {
          callbackSetList([{ title: "已取消", arg: { type: "noop" } }]);
        }
      },
    },
  },

  // ─── wb-checkin：签到领积分 ───

  "wb-checkin": {
    mode: "list",
    args: {
      placeholder: "回车执行全账号签到（接口幂等，重复调用无害）",
      enter(action, callbackSetList) {
        lastItems = [{ title: "⏳ 正在签到…", arg: { type: "noop" } }];
        callbackSetList(lastItems);
        runCli(["--checkin"])
          .then((out) => {
            const r = jparse(out) || [];
            if (!r.length) {
              lastItems = [
                {
                  title: "没有需要签到的账号",
                  description:
                    "自动领取开关未开启（GUI 里开过），或今日已全部领取",
                  arg: { type: "noop" },
                },
              ];
              callbackSetList(lastItems);
              return;
            }
            lastItems = r.map((x) => ({
              title:
                short(x.uid) +
                "  " +
                (x.ok
                  ? x.status === "claimed"
                    ? `✓ 领取 ${x.credit} 积分`
                    : x.status === "already"
                    ? "今日已领"
                    : x.status
                  : "❌ 失败"),
              description: x.msg || "",
              arg: { type: "noop" },
            }));
            callbackSetList(lastItems);
          })
          .catch((e) => {
            lastItems = [errItem(e)];
            callbackSetList(lastItems);
          });
      },
      select(item, callbackSetList) {
        callbackSetList(lastItems.length ? lastItems : [item]);
      },
    },
  },
};
