import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react';
import {
  wbCheckinAutoGet,
  wbCheckinAutoSet,
  wbCheckinClaim,
  wbCheckinStatus,
  wbCreditByAccount,
  wbCreditSetBalance,
  wbCreditSetTotal,
  wbCreditSummary,
  wbDeleteSlot,
  wbImportLegacy,
  wbOverview,
  wbRenameSlot,
  wbSaveSlot,
  wbSessionCopy,
  wbSessionList,
  wbSwitch,
  zcodeDeleteSlot,
  zcodeLaunch,
  zcodeOverview,
  zcodeQuota,
  zcodeRenameSlot,
  zcodeSaveSlot,
  zcodeSwitch,
  type SwitchReport,
  type SessionCopyReport,
  type WbAccountCredit,
  type WbCheckinResult,
  type WbCheckinStatus,
  type WbCreditSummary,
  type WbOverview,
  type WbSessionInfo,
  type ZcodeAccountOverview,
  type ZcodeQuota,
  type ZcodeSwitchReport,
} from '../api';
import { PageHeader } from '../common/components/PageHeader';
import { ConfirmDialog, InputDialog } from '../common/components/ConfirmDialog';
import { toast } from '../stores/ui';
import { isAgentEnabled, useEnabledAgents } from '../stores/plugins';

const mask = (s: string) => (s.length > 8 ? `${s.slice(0, 6)}…` : s || '—');

/** 签到状态当日缓存（wb_checkin_status 逐账号拉起 WB 解密 + HTTP，秒级；先渲染缓存） */
const CHECKIN_CACHE_KEY = 'agenthub.checkinCache.v1';
const todayKey = () => {
  const d = new Date();
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
};

type WbConfirmState =
  | { kind: 'switch'; slotId: string; label: string }
  | { kind: 'delete'; slotId: string; label: string }
  | null;
/** 切号 + 会话复制弹窗（P1）：选择要带走到目标账号的会话 */
type SwitchDialogState = { slotId: string; label: string } | null;
type ZcConfirmState = { kind: 'switch' | 'delete'; slotId: string; label: string } | null;
/** 积分编辑：目标槽位 + 当前余额（null = 从未设置） */
type BalanceEditState = { uid: string; label: string; balance: number | null } | null;

const pctColor = (pct: number) =>
  pct > 80 ? 'var(--color-danger)' : pct > 50 ? 'var(--color-warn, #d9a441)' : 'var(--color-primary)';

const fmtInt = (n: number) => Math.round(n).toLocaleString();

/** 额度条（ZCode 五小时/周窗口共用） */
function QuotaBar({ label, w }: { label: string; w: ZcodeQuota['h5'] }) {
  const pct = Math.min(100, Math.max(0, w.used_pct));
  return (
    <div style={{ marginBottom: 8 }}>
      <div className="flex items-center" style={{ gap: 8 }}>
        <b style={{ fontSize: 12, color: 'var(--color-text-2)', flex: 'none' }}>{label}</b>
        <span
          style={{
            fontSize: 12,
            fontWeight: 800,
            fontFamily: 'Consolas, monospace',
            color: pctColor(pct),
          }}
        >
          {pct}%
        </span>
        <span style={{ flex: 1 }}>
          <span className="bar">
            <i style={{ width: `${pct}%`, background: pctColor(pct) }} />
          </span>
        </span>
        <span style={{ fontSize: 10.5, color: 'var(--color-text-3)', flex: 'none' }}>
          {w.used.toLocaleString()} / {w.total.toLocaleString()} 分
          {w.reset_at && <> · 重置 {w.reset_at}</>}
        </span>
      </div>
    </div>
  );
}

/**
 * 折叠动画容器：grid-template-rows 0fr↔1fr 过渡（高度平滑收放）。
 * 包裹整个槽位卡片网格 —— 收起时分组只保留状态条与操作按钮。
 */
function Collapse({ open, children }: { open: boolean; children: ReactNode }) {
  return (
    <div
      aria-hidden={!open}
      style={{
        gridColumn: '1/3',
        display: 'grid',
        gridTemplateRows: open ? '1fr' : '0fr',
        transition: 'grid-template-rows 0.35s ease',
      }}
    >
      <div style={{ overflow: 'hidden', minHeight: 0 }}>
        <div
          style={{
            display: 'grid',
            gridTemplateColumns: '1fr 1fr',
            gap: 10,
            paddingTop: open ? 10 : 0,
            opacity: open ? 1 : 0,
            transition: 'padding-top 0.35s ease, opacity 0.25s ease',
          }}
        >
          {children}
        </div>
      </div>
    </div>
  );
}

/** 卡片标题：点击变输入框，回车/失焦确认改名，Esc 取消（改名按钮已移除） */
function SlotTitle(props: {
  label: string;
  editing: boolean;
  onStart: () => void;
  onCommit: (label: string) => void;
  onCancel: () => void;
}) {
  const { label, editing, onStart, onCommit, onCancel } = props;
  if (!editing) {
    return (
      <b style={{ fontSize: 13, cursor: 'text' }} title="点击改名（回车确认，Esc 取消）" onClick={onStart}>
        {label}
      </b>
    );
  }
  return (
    <input
      autoFocus
      defaultValue={label}
      style={{
        fontSize: 13, fontWeight: 700, width: 150, padding: '2px 6px', borderRadius: 6,
        border: '1px solid var(--color-primary)', background: 'var(--color-bg)',
        color: 'var(--color-text-1)', outline: 'none',
      }}
      onKeyDown={(e) => {
        if (e.key === 'Enter') onCommit(e.currentTarget.value.trim());
        else if (e.key === 'Escape') onCancel();
      }}
      onBlur={(e) => onCommit(e.currentTarget.value.trim())}
    />
  );
}

/** Account 岛 —— WorkBuddy / ZCode 双分组：槽位切换 + WB 剩余积分 + ZCode 官方额度 */
export function AccountsPage() {
  // ── WorkBuddy ──
  const [data, setData] = useState<WbOverview | null>(null);
  const [report, setReport] = useState<SwitchReport | null>(null);
  const [credit, setCredit] = useState<WbCreditSummary | null>(null);
  // 按账户（uid）积分用量：WB 槽位卡片【当日使用 / 账户剩余】
  const [creditAcct, setCreditAcct] = useState<WbAccountCredit[]>([]);
  // 每日签到状态（uid → 今日是否已领）；不在 10s 轮询里，切号/手动刷新时查询
  const [checkins, setCheckins] = useState<WbCheckinStatus[]>([]);
  const [autoCheckin, setAutoCheckin] = useState(true);
  const [claiming, setClaiming] = useState<string | null>(null);
  const [editingBalance, setEditingBalance] = useState<BalanceEditState>(null);
  const [confirm, setConfirm] = useState<WbConfirmState>(null);
  const [naming, setNaming] = useState(false);
  const [editingTotal, setEditingTotal] = useState(false);

  // ── 切号弹窗 ──
  const [switchDlg, setSwitchDlg] = useState<SwitchDialogState>(null);
  const [killOn, setKillOn] = useState(false);

  // ── 会话复制弹窗（独立于切号；源 = 发起卡片账号，目标可多选） ──
  const [copyDlg, setCopyDlg] = useState<{ uid: string; label: string } | null>(null);
  const [sessList, setSessList] = useState<WbSessionInfo[] | null>(null);
  const [sessLoading, setSessLoading] = useState(false);
  const [sessSel, setSessSel] = useState<string[]>([]);
  const [sessSearch, setSessSearch] = useState('');
  const [copyTargets, setCopyTargets] = useState<string[]>([]);
  const [copyKillOn, setCopyKillOn] = useState(false);
  const [copyReport, setCopyReport] = useState<SessionCopyReport | null>(null);
  const [copyBusy, setCopyBusy] = useState(false);

  // ── ZCode ──
  const [zc, setZc] = useState<ZcodeAccountOverview | null>(null);
  const [zcReport, setZcReport] = useState<ZcodeSwitchReport | null>(null);
  const [zcQuotaData, setZcQuotaData] = useState<ZcodeQuota | null>(null);
  const [zcQuotaErr, setZcQuotaErr] = useState<string | null>(null);
  const [zcQuotaLoading, setZcQuotaLoading] = useState(false);
  const [zcConfirm, setZcConfirm] = useState<ZcConfirmState>(null);
  const [zcNaming, setZcNaming] = useState(false);
  // 分组展开/收起：收起只显示当前生效账户（>1 个槽位时才出现开关）
  const [wbOpen, setWbOpen] = useState(true);
  const [zcOpen, setZcOpen] = useState(true);
  // 槽位改名：agent = 目标分组，slotId + 当前名（预填输入框）
  const [renaming, setRenaming] = useState<
    { agent: 'wb' | 'zc'; slotId: string; label: string } | null
  >(null);

  const [busy, setBusy] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  // 插件门控：停用端的分组置灰（槽位仅展示不可操作，切换/启动/保存等全部禁用）
  const enabledIds = useEnabledAgents();
  const wbOff = !isAgentEnabled(enabledIds, 'workbuddy');
  const zcOff = !isAgentEnabled(enabledIds, 'zcode');
  const offStyle = { opacity: 0.45, pointerEvents: 'none' as const };

  const refresh = useCallback(async (): Promise<WbOverview | null> => {
    setError(null);
    try {
      const d = await wbOverview();
      setData(d);
      return d;
    } catch (e) {
      setError(String(e));
      return null;
    }
  }, []);

  const refreshCredit = useCallback(async () => {
    try {
      setCredit(await wbCreditSummary());
      setCreditAcct(await wbCreditByAccount());
    } catch {
      /* 浏览器预览等场景忽略 */
    }
  }, []);

  /** 签到状态查询（登录态文件 + 槽位逐账号；挂载/切号/手动刷新时调用）。
   *  Rust 侧逐账号 ELECTRON_RUN_AS_NODE 拉起 WB 解密 token + HTTP 查询，秒级耗时；
   *  先渲染当日 localStorage 缓存立即出完整卡片，后台刷新完成后替换。 */
  const refreshCheckins = useCallback(async () => {
    try {
      const cached = localStorage.getItem(CHECKIN_CACHE_KEY);
      if (cached) {
        const v = JSON.parse(cached) as { date: string; list: WbCheckinStatus[] };
        if (v.date === todayKey() && Array.isArray(v.list)) setCheckins(v.list);
      }
    } catch {
      /* 缓存损坏忽略 */
    }
    try {
      const list = await wbCheckinStatus();
      setCheckins(list);
      try {
        localStorage.setItem(CHECKIN_CACHE_KEY, JSON.stringify({ date: todayKey(), list }));
      } catch {
        /* 配额满忽略 */
      }
    } catch {
      /* 浏览器预览等场景忽略 */
    }
  }, []);

  /** 手动领取某账号每日积分 */
  const doClaim = useCallback(
    async (uid: string) => {
      setClaiming(uid);
      try {
        const r: WbCheckinResult = await wbCheckinClaim(uid);
        if (r.status === 'claimed') toast(`已领取 ${r.credit} 积分`);
        else if (r.status === 'already') toast('今日已领取过');
        else toast(r.msg || '领取失败', 'err');
      } catch (e) {
        toast(String(e), 'err');
      } finally {
        setClaiming(null);
      }
      await refreshCheckins();
      await refreshCredit();
    },
    [refreshCheckins, refreshCredit],
  );

  const toggleAutoCheckin = async () => {
    const next = !autoCheckin;
    setAutoCheckin(next);
    try {
      await wbCheckinAutoSet(next);
      toast(next ? '启动自动签到已开启' : '启动自动签到已关闭');
    } catch (e) {
      setAutoCheckin(!next);
      toast(String(e), 'err');
    }
  };

  const refreshZc = useCallback(async () => {
    try {
      setZc(await zcodeOverview());
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const refreshZcQuota = useCallback(async () => {
    setZcQuotaLoading(true);
    setZcQuotaErr(null);
    try {
      setZcQuotaData(await zcodeQuota());
    } catch (e) {
      setZcQuotaErr(String(e));
    } finally {
      setZcQuotaLoading(false);
    }
  }, []);

  // ZCode 插件停用时额度查询属「唤起 agent 工具」，直接跳过
  useEffect(() => {
    if (enabledIds !== null && enabledIds.includes('zcode') && zcQuotaData === null && zcQuotaErr === null) {
      void refreshZcQuota();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [enabledIds]);

  useEffect(() => {
    void refresh();
    void refreshCredit();
    void refreshCheckins();
    void wbCheckinAutoGet().then(setAutoCheckin).catch(() => {});
    void refreshZc();
    if (isAgentEnabled(enabledIds, 'zcode')) void refreshZcQuota();
    // WB 侧账号/进程可能随时变化（手动切号、启动退出），轮询保持实时
    // （签到状态走网络按账号查询，只在上面的挂载/切号/手动刷新时更新，不进轮询）
    // 可见性感知：窗口隐藏（关到托盘）暂停轮询，避免后台定时器积压、恢复前台时齐发卡顿；
    // 30s 降频（Rust 侧 process_running 已有 3s 缓存，切号检测仍及时）
    const POLL_MS = 30_000;
    const tick = () => {
      if (document.hidden) return;
      void refresh();
      void refreshCredit();
      void refreshZc();
    };
    const onVis = () => {
      if (!document.hidden) tick();
    };
    const t = setInterval(tick, POLL_MS);
    document.addEventListener('visibilitychange', onVis);
    return () => {
      clearInterval(t);
      document.removeEventListener('visibilitychange', onVis);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [refresh, refreshCredit, refreshCheckins, refreshZc, refreshZcQuota, enabledIds]);

  const manualRefresh = async () => {
    setRefreshing(true);
    const d = await refresh();
    await refreshCredit();
    await refreshCheckins();
    await refreshZc();
    setRefreshing(false);
    if (d) toast('账号状态已刷新');
    else toast('刷新失败', 'err');
  };

  // ── WB 槽位操作 ──
  const doSaveCurrent = async (label: string) => {
    setBusy(true);
    try {
      await wbSaveSlot(label);
      setMessage(`已保存当前登录为「${label}」`);
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const doSwitch = async (slotId: string, label: string, confirmKill: boolean) => {
    setBusy(true);
    setError(null);
    try {
      const r = await wbSwitch(slotId, confirmKill);
      setReport(r);
      if (r.ok) {
        const killed = confirmKill;
        setMessage(`已切换到「${label}」${killed ? '（WorkBuddy 已关闭并重启）' : ''}`);
        toast(`已切换到「${label}」${killed ? '，WorkBuddy 已重启' : ''}`);
        await refresh();
        // 切号后自动查询该账号今日签到状态；未领取则自动补领（接口幂等）
        try {
          const cs = await wbCheckinStatus();
          setCheckins(cs);
          const d = await refresh();
          const uid = d?.current?.uid;
          const st = cs.find((x) => x.uid === uid);
          if (uid && st && st.today_claimed === false) {
            const cr: WbCheckinResult = await wbCheckinClaim(uid);
            if (cr.status === 'claimed') toast(`已自动为新账号领取 ${cr.credit} 积分`);
          }
          setCheckins(await wbCheckinStatus());
          await refreshCredit();
        } catch {
          /* 签到查询失败不影响切换流程 */
        }
      }
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  /** 打开切号确认弹窗（会话复制已抽离为卡片级独立弹窗） */
  const openSwitchDialog = (slotId: string, label: string) => {
    setSwitchDlg({ slotId, label });
    setKillOn(false);
  };

  /** 打开会话复制弹窗：源 = 发起卡片账号，加载该账号会话 + 其余账号作为候选目标 */
  const openCopyDialog = (uid: string, label: string) => {
    setCopyDlg({ uid, label });
    setSessSel([]);
    setSessSearch('');
    setCopyTargets([]);
    setCopyKillOn(false);
    setCopyReport(null);
    setSessList(null);
    setSessLoading(true);
    void wbSessionList(uid)
      .then(setSessList)
      .catch(() => setSessList([]))
      .finally(() => setSessLoading(false));
  };

  /** 执行复制：空选提示 → WB 运行中需勾选强杀 → 调 IPC */
  const doCopySessions = async () => {
    if (!copyDlg) return;
    if (sessSel.length === 0) {
      toast('请先勾选要复制的会话', 'err');
      return;
    }
    if (copyTargets.length === 0) {
      toast('请至少勾选一个目标账号', 'err');
      return;
    }
    if (data?.process_running && !copyKillOn) {
      toast('复制需先关闭 WorkBuddy，请勾选「强制结束」', 'err');
      return;
    }
    setCopyBusy(true);
    try {
      const r = await wbSessionCopy(sessSel, copyTargets, Boolean(data?.process_running && copyKillOn));
      setCopyReport(r);
      if (r.ok) {
        const n = r.targets.reduce((acc, t) => acc + t.results.filter((x) => x.status === 'copied').length, 0);
        toast(`已复制 ${n} 份会话${r.restarted ? '，WorkBuddy 已重新启动' : ''}`);
        // 成功即自动关闭弹窗（失败才留在弹窗内看报告）
        setCopyDlg(null);
        setCopyReport(null);
        await refresh();
      } else if (!r.need_kill && r.error) {
        toast(r.error, 'err');
      }
    } catch (e) {
      toast(String(e), 'err');
    } finally {
      setCopyBusy(false);
    }
  };

  const selectable = (s: WbSessionInfo) => s.jsonl_ok && !s.claw;

  const importLegacy = async () => {
    setBusy(true);
    try {
      const n = await wbImportLegacy();
      setMessage(n > 0 ? `从 ~/.workbuddy-launcher 导入 ${n} 个槽位` : '没有可导入的旧槽位');
      toast(n > 0 ? `导入 ${n} 个旧槽位` : '没有可导入的旧槽位');
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const saveTotal = async (v: { total?: string }) => {
    setEditingTotal(false);
    const n = Number((v.total ?? '').trim());
    if (!Number.isFinite(n)) return;
    try {
      await wbCreditSetTotal(n);
      toast(n > 0 ? `已设每月积分总额 ${n.toLocaleString()}` : '已清除总额度');
      await refreshCredit();
    } catch (e) {
      setError(String(e));
    }
  };

  /** 保存某账户的当前积分快照（0/空 = 清除该账户的余额设置） */
  const saveBalance = async (uid: string, raw: string) => {
    setEditingBalance(null);
    const t = raw.trim();
    if (!t) return;
    const n = Number(t);
    if (!Number.isFinite(n)) {
      toast('积分必须是数字', 'err');
      return;
    }
    try {
      await wbCreditSetBalance(uid, n);
      toast(n > 0 ? `已设该账户当前积分 ${n.toLocaleString()}（此后用量/领取自动增减）` : '已清除该账户余额设置');
      await refreshCredit();
    } catch (e) {
      setError(String(e));
    }
  };

  // ── ZCode 槽位操作 ──
  const zcDoSave = async (label: string) => {
    setBusy(true);
    try {
      await zcodeSaveSlot(label);
      setMessage(`ZCode 槽位「${label}」已保存`);
      toast(`ZCode 槽位「${label}」已保存`);
      await refreshZc();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const zcDoSwitch = async (slotId: string, label: string) => {
    setBusy(true);
    setError(null);
    try {
      const r = await zcodeSwitch(slotId);
      setZcReport(r);
      if (r.ok) {
        setMessage(`已切换到「${label}」，下次启动 ZCode 生效`);
        toast(`已切换到「${label}」`);
      }
      await refreshZc();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const zcLaunch = async () => {
    try {
      await zcodeLaunch();
      toast('ZCode 已启动');
    } catch (e) {
      toast(String(e), 'err');
    }
    await refreshZc();
  };

  const wbSlots = data?.slots ?? [];
  const zcSlots = zc?.slots ?? [];

  // 防回车 + 失焦双触发提交
  const renameLock = useRef(false);

  /** 标题内联改名提交（回车/失焦共用；空值或未变化则视为取消） */
  const commitRename = (
    t: { agent: 'wb' | 'zc'; slotId: string; label: string },
    label: string,
  ) => {
    if (renameLock.current) return;
    renameLock.current = true;
    setRenaming(null);
    if (label && label !== t.label) void doRename(t, label);
    setTimeout(() => {
      renameLock.current = false;
    }, 0);
  };

  /** Esc 取消标题改名 */
  const cancelRename = () => {
    if (renameLock.current) return;
    renameLock.current = true;
    setRenaming(null);
    setTimeout(() => {
      renameLock.current = false;
    }, 0);
  };

  /** 槽位改名（WB / ZCode 共用） */
  const doRename = async (
    t: { agent: 'wb' | 'zc'; slotId: string; label: string },
    label: string,
  ) => {
    setBusy(true);
    try {
      if (t.agent === 'wb') await wbRenameSlot(t.slotId, label);
      else await zcodeRenameSlot(t.slotId, label);
      toast(`已改名为「${label}」`);
      await refresh();
      await refreshZc();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  /** WB 槽位卡片（当前生效 = uid 与当前登录态一致，绿色主题） */
  const wbCard = (s: (typeof wbSlots)[number]) => {
    const isCurrent = data?.auth_found && s.uid && s.uid === data.current.uid;
    return (
      <div
        key={s.id}
        className="card hover-zoom"
        style={{
          padding: 14,
          border: isCurrent ? '1.5px solid var(--color-agent-wb)' : undefined,
          background: isCurrent
            ? 'color-mix(in srgb, var(--color-agent-wb) 6%, var(--color-surface))'
            : undefined,
        }}
      >
        <div className="flex items-center justify-between">
          <SlotTitle
            label={s.label}
            editing={renaming?.agent === 'wb' && renaming.slotId === s.id}
            onStart={() => setRenaming({ agent: 'wb', slotId: s.id, label: s.label })}
            onCommit={(label) => commitRename({ agent: 'wb', slotId: s.id, label: s.label }, label)}
            onCancel={cancelRename}
          />
          {isCurrent ? (
            <span className="chip wb">当前生效</span>
          ) : (
            <span className="chip off">槽位 {s.id.startsWith('legacy-') ? '旧导入' : 'A/B'}</span>
          )}
        </div>
        <div style={{ color: 'var(--color-text-3)', fontSize: 11, margin: '6px 0' }}>
          uid {mask(s.uid)} · uin {mask(s.uin)} · 保存于 {s.saved_at}
          {!s.file_ok && <span style={{ color: 'var(--color-danger)' }}> · ⚠ 快照文件缺失</span>}
        </div>
        {/* 积分：当日使用 / 账户剩余；账户单独设置过当前积分则按余额口径显示 */}
        {(() => {
          const c = creditAcct.find((x) => x.uid === s.uid);
          const st = checkins.find((x) => x.uid === s.uid);
          // 该账号尚无任何用量/余额记录：只渲染签到状态 chip
          if (!c) {
            if (!st) return null;
            return (
              <div style={{ fontSize: 11, margin: '2px 0 6px', color: 'var(--color-text-2)' }}>
                {st.today_claimed === true ? (
                  <span className="chip wb" title={`本地记录 ${st.last_date} +${fmtInt(st.last_credit)}`}>
                    今日已领{st.last_credit > 0 ? ` +${fmtInt(st.last_credit)}` : ''}
                  </span>
                ) : st.today_claimed === false ? (
                  <span className="chip warn">今日未领</span>
                ) : (
                  <span className="chip off" title={st.error ?? '签到状态未知'}>
                    签到 ?
                  </span>
                )}
              </div>
            );
          }
          return (
            <div style={{ fontSize: 11, margin: '2px 0 6px', color: 'var(--color-text-2)' }}>
              当日使用积分{' '}
              <b style={{ fontFamily: 'Consolas, monospace' }}>{fmtInt(c.today_used)}</b>
              {c.balance != null ? (
                <>
                  {' · '}账户剩余积分{' '}
                  <b
                    style={{
                      fontFamily: 'Consolas, monospace',
                      color:
                        c.balance > 0 && c.used_since_set / c.balance > 0.8
                          ? 'var(--color-danger)'
                          : 'var(--color-primary)',
                    }}
                  >
                    {fmtInt(c.remaining)}
                  </b>
                  <span
                    style={{ color: 'var(--color-text-3)', cursor: 'pointer' }}
                    title={`手动设置的当前积分快照 ${fmtInt(c.balance)}（${c.balance_set_at}），领取的积分自动入账`}
                    onClick={() => setEditingBalance({ uid: s.uid, label: s.label, balance: c.balance })}
                  >
                    {' '}(设置于 {c.balance_set_at.slice(0, 10)} · 点此编辑)
                  </span>
                </>
              ) : credit && credit.monthly_total > 0 ? (
                <>
                  {' · '}账户剩余积分{' '}
                  <b
                    style={{
                      fontFamily: 'Consolas, monospace',
                      color:
                        credit.monthly_total > 0 && c.month_used / credit.monthly_total > 0.8
                          ? 'var(--color-danger)'
                          : 'var(--color-primary)',
                    }}
                  >
                    {fmtInt(c.remaining)}
                  </b>
                  <span style={{ color: 'var(--color-text-3)' }}>
                    {' '}/ {fmtInt(credit.monthly_total)}
                  </span>
                </>
              ) : (
                <span style={{ color: 'var(--color-text-3)' }}>
                  {' '}· 本月已用 {fmtInt(c.month_used)}（设总额度或单独设置积分后显示剩余）
                </span>
              )}
              {/* 每日签到状态 chip */}
              {st && (
                <>
                  {' · '}
                  {st.today_claimed === true ? (
                    <span className="chip wb" title={`本地记录 ${st.last_date} +${fmtInt(st.last_credit)}`}>
                      今日已领{st.last_credit > 0 ? ` +${fmtInt(st.last_credit)}` : ''}
                    </span>
                  ) : st.today_claimed === false ? (
                    <span className="chip warn">今日未领</span>
                  ) : (
                    <span className="chip off" title={st.error ?? '签到状态未知'}>
                      签到 ?
                    </span>
                  )}
                </>
              )}
            </div>
          );
        })()}
        <div className="flex" style={{ gap: 8, flexWrap: 'wrap' }}>
          <button
            className="btn gh"
            title="把该账号的会话复制给其他 WB 账号（可一次勾选多个目标）"
            onClick={() => openCopyDialog(s.uid, s.label)}
            disabled={busy}
          >
            ⧉ 复制会话
          </button>
          {isCurrent ? (
            <span className="btn gh" style={{ opacity: 0.6, cursor: 'default' }}>当前账号</span>
          ) : (
            <button className="btn wbp" onClick={() => openSwitchDialog(s.id, s.label)} disabled={busy || !s.file_ok}>
              ⇄ 切换到此账号
            </button>
          )}
          <button
            className="btn gh"
            title={
              data?.process_running
                ? 'WorkBuddy 已在运行'
                : s.file_ok
                  ? '以该账号启动 WorkBuddy（若非当前登录态会先切换）'
                  : '槽位文件缺失'
            }
            onClick={() => void doSwitch(s.id, s.label, false)}
            disabled={busy || !s.file_ok || data?.process_running}
          >
            ▶ 启动
          </button>

          {/* 每日领取 100 积分（走官方签到接口，幂等；用该账号槽位登录态直签，无需切换） */}
          {(() => {
            const st = checkins.find((x) => x.uid === s.uid);
            const claimed = st?.today_claimed === true;
            return (
              <button
                className={claimed ? 'btn gh' : 'btn p'}
                title={
                  claimed
                    ? `今日已领取${st?.last_credit ? ` +${fmtInt(st.last_credit)}` : ''}`
                    : '领取该账号的每日签到积分（第 1-6 天 +100，第 7 天 +1000）'
                }
                onClick={() => void doClaim(s.uid)}
                disabled={busy || claiming === s.uid || claimed}
              >
                <span className={claiming === s.uid ? 'spinning' : ''} style={{ display: 'inline-block' }}>
                  {claiming === s.uid ? '↻' : '🎁'}
                </span>{' '}
                {claimed ? '今日已领' : '领每日积分'}
              </button>
            );
          })()}
          {(() => {
            const c = creditAcct.find((x) => x.uid === s.uid);
            return (
              <button
                className="btn gh"
                title="单独设置/编辑该账户的当前积分（作为余额基准，后续用量与领取自动增减）"
                onClick={() => setEditingBalance({ uid: s.uid, label: s.label, balance: c?.balance ?? null })}
                disabled={busy}
              >
                {'✎ 积分'}
              </button>
            );
          })()}
          <button className="btn gh" onClick={() => setConfirm({ kind: 'delete', slotId: s.id, label: s.label })} disabled={busy}>
            删除
          </button>
        </div>
      </div>
    );
  };

  /** ZCode 槽位卡片（当前生效 = 快照与当前登录态一致，蓝色主题） */
  const zcCard = (s: (typeof zcSlots)[number]) => {
    const isCurrent = s.active;
    return (
      <div
        key={s.id}
        className="card hover-zoom"
        style={{
          padding: 14,
          border: isCurrent ? '1.5px solid var(--color-agent-zcode)' : undefined,
          background: isCurrent
            ? 'color-mix(in srgb, var(--color-agent-zcode) 8%, var(--color-surface))'
            : undefined,
        }}
      >
        <div className="flex items-center justify-between">
          <SlotTitle
            label={s.label}
            editing={renaming?.agent === 'zc' && renaming.slotId === s.id}
            onStart={() => setRenaming({ agent: 'zc', slotId: s.id, label: s.label })}
            onCommit={(label) => commitRename({ agent: 'zc', slotId: s.id, label: s.label }, label)}
            onCancel={cancelRename}
          />
          {isCurrent ? (
            <span className="chip zc">当前生效</span>
          ) : (
            <span className="chip off">槽位</span>
          )}
        </div>
        <div style={{ color: 'var(--color-text-3)', fontSize: 11, margin: '6px 0' }}>
          保存于 {s.saved_at}
          {!s.file_ok && <span style={{ color: 'var(--color-danger)' }}> · ⚠ 快照文件缺失</span>}
        </div>
        {/* 额度完整信息：当前生效卡（Key 属于当前登录账号，其余槽位无从查询） */}
        {isCurrent && (
          <div style={{ margin: '4px 0 4px' }}>
            {zcQuotaData ? (
              <>
                {zcQuotaData.plan_level && (
                  <div style={{ fontSize: 10.5, color: 'var(--color-text-3)', marginBottom: 4 }}>
                    套餐 {zcQuotaData.plan_level} · 官方接口实时数据
                  </div>
                )}
                <QuotaBar label="五小时额度" w={zcQuotaData.h5} />
                <QuotaBar label="周额度" w={zcQuotaData.week} />
              </>
            ) : zcQuotaLoading ? (
              <div style={{ fontSize: 11, color: 'var(--color-text-3)', margin: '6px 0' }}>
                正在查询官方额度…
              </div>
            ) : (
              <div style={{ fontSize: 11, color: 'var(--color-text-3)', margin: '6px 0' }}>
                {zcQuotaErr ? `额度查询失败：${zcQuotaErr}` : '点「刷新额度」查询官方实时用量'}
              </div>
            )}
          </div>
        )}
        <div className="flex" style={{ gap: 8 }}>
          {isCurrent ? (
            <span className="btn gh" style={{ opacity: 0.6, cursor: 'default' }}>当前账号</span>
          ) : (
            <button
              className="btn zcp"
              title="恢复该槽位的 ZCode 登录态（下次启动 ZCode 生效）"
              onClick={() => setZcConfirm({ kind: 'switch', slotId: s.id, label: s.label })}
              disabled={busy || !s.file_ok}
            >
              ⇄ 切换到此账号
            </button>
          )}
          <button
            className="btn gh"
            title={zc?.running ? 'ZCode 已在运行' : '启动 ZCode 桌面（使用当前登录态）'}
            onClick={() => void zcLaunch()}
            disabled={busy || zc?.running}
          >
            ▶ 启动
          </button>
          <button
            className="btn gh"
            onClick={() => setZcConfirm({ kind: 'delete', slotId: s.id, label: s.label })}
            disabled={busy}
          >
            删除
          </button>
        </div>
      </div>
    );
  };

  return (
    <div>
      <PageHeader
        title={`账号（WorkBuddy ${wbSlots.length} · ZCode ${zcSlots.length} 槽位）`}
        sub="登录态只做文件级复制永不读内容 · WB 剩余积分 = 每月总额度 − 本月已用（与账单同源）· ZCode 额度走官方接口"
      />

      <div className="mb-3 flex items-center" style={{ gap: 8 }}>
        <span style={{ flex: 1 }} />
        {message && <span className="chip ok">{message}</span>}
        <button className="btn gh" onClick={manualRefresh} disabled={refreshing}>
          <span className={refreshing ? 'spinning' : ''} style={{ display: 'inline-block' }}>↻</span> 刷新
        </button>
      </div>

      {error && (
        <div
          style={{
            fontSize: 12,
            color: 'var(--color-danger)',
            background: 'color-mix(in srgb, var(--color-danger) 8%, transparent)',
            borderRadius: 10,
            padding: '8px 12px',
            marginBottom: 12,
            whiteSpace: 'pre-wrap',
          }}
        >
          {error}
        </div>
      )}

      {/* ═══ WorkBuddy 分组（操作按钮并入标题行） ═══ */}
      <div className="flex items-center" style={{ gap: 8, margin: '4px 0 8px', flexWrap: 'wrap' }}>
        <b style={{ fontSize: 13.5, color: 'var(--color-agent-wb)' }}>WorkBuddy</b>
        <span className="chip wb">多账号槽位</span>
        {wbOff && <span className="chip warn" title="到 Plugin 岛开启 WorkBuddy 插件后可操作">插件已停用 · 仅展示</span>}
        <span style={{ flex: 1 }} />
        <button className="btn gh" onClick={importLegacy} disabled={busy || wbOff}>⇪ 导入旧槽位</button>
        <button className="btn wbp" onClick={() => setNaming(true)} disabled={busy || wbOff}>
          ⤓ 保存当前登录为槽位
        </button>
        <button className="btn gh" onClick={() => setEditingTotal(true)} disabled={busy || wbOff}>
          {credit && credit.monthly_total > 0 ? '修改总额度' : '设置每月总额度'}
        </button>
        <span
          className={autoCheckin ? 'chip wb' : 'chip off'}
          style={{ cursor: 'pointer', userSelect: 'none' }}
          title="AgentHub 启动时自动为全部 WB 账号领取每日积分（接口幂等，不会多领）"
          onClick={() => void toggleAutoCheckin()}
        >
          {autoCheckin ? '启动自动签到 ✓' : '启动自动签到 ✕'}
        </span>
        {wbSlots.length > 1 && (
          <span
            className="chip off"
            style={{ cursor: 'pointer', userSelect: 'none' }}
            onClick={() => setWbOpen((o) => !o)}
          >
            {wbOpen ? '收起 ▴' : `展开全部 ▾ · ${wbSlots.length} 槽位`}
          </span>
        )}
      </div>

      {/* WB 切换结果流程条 */}
      {report && (
        <div
          className="card"
          style={{
            padding: 14,
            marginBottom: 12,
            borderColor: report.ok ? 'var(--color-agent-wb)' : 'var(--color-danger)',
          }}
        >
          <b style={{ fontSize: 12, color: report.ok ? 'var(--color-agent-wb)' : 'var(--color-danger)' }}>
            {report.ok ? '✓ WB 切换成功' : '✕ WB 切换失败（已自动回滚）'}
          </b>
          <div style={{ fontSize: 11.5, color: 'var(--color-text-2)', marginTop: 6, whiteSpace: 'pre-wrap' }}>
            {report.steps.map((s, i) => `${i + 1}. ${s}`).join('\n')}
            {report.error && `\n错误：${report.error}`}
          </div>
        </div>
      )}

      {/* WB 槽位卡片：随展开/收起动画整体收放（收起只留分组状态条与操作按钮）；插件停用整体置灰 */}
      <div className="grid grid-cols-2 gap-2.5" style={{ marginBottom: 20, ...(wbOff ? offStyle : {}) }}>
        {wbSlots.length === 0 ? (
          <div
            className="card"
            style={{ padding: 18, color: 'var(--color-text-3)', fontSize: 12, gridColumn: '1/3' }}
          >
            还没有槽位 —— 登录 WB 后点「保存当前登录为槽位」，或点「导入旧槽位」从
            ~/.workbuddy-launcher 迁移。
          </div>
        ) : (
          <Collapse open={wbOpen}>{wbSlots.map((s) => wbCard(s))}</Collapse>
        )}
      </div>

      {/* 分组分隔线 */}
      <div style={{ borderTop: '1px solid var(--color-border)', margin: '2px 0 14px' }} />

      {/* ═══ ZCode 分组（操作按钮并入标题行） ═══ */}
      <div className="flex items-center" style={{ gap: 8, margin: '4px 0 8px', flexWrap: 'wrap' }}>
        <b style={{ fontSize: 13.5, color: 'var(--color-agent-zcode)' }}>ZCode</b>
        <span className="chip zc">快速切号</span>
        {zcOff && <span className="chip warn" title="到 Plugin 岛开启 ZCode 插件后可操作">插件已停用 · 仅展示</span>}
        <span style={{ flex: 1 }} />
        <button
          className="btn gh"
          onClick={() => void refreshZcQuota()}
          disabled={zcQuotaLoading || zcOff}
        >
          <span className={zcQuotaLoading ? 'spinning' : ''} style={{ display: 'inline-block' }}>↻</span> 刷新额度
        </button>
        <button className="btn zcp" onClick={() => setZcNaming(true)} disabled={busy || zcOff}>
          ⤓ 保存当前登录为槽位
        </button>
        {zcSlots.length > 1 && (
          <span
            className="chip off"
            style={{ cursor: 'pointer', userSelect: 'none' }}
            onClick={() => setZcOpen((o) => !o)}
          >
            {zcOpen ? '收起 ▴' : `展开全部 ▾ · ${zcSlots.length} 槽位`}
          </span>
        )}
      </div>

      {/* ZCode 切换结果流程条 */}
      {zcReport && (
        <div
          className="card"
          style={{
            padding: 14,
            marginBottom: 12,
            borderColor: zcReport.ok ? 'var(--color-agent-zcode)' : 'var(--color-danger)',
          }}
        >
          <b style={{ fontSize: 12, color: zcReport.ok ? 'var(--color-agent-zcode)' : 'var(--color-danger)' }}>
            {zcReport.ok ? '✓ ZCode 切换成功' : '✕ ZCode 切换失败'}
          </b>
          <div style={{ fontSize: 11.5, color: 'var(--color-text-2)', marginTop: 6, whiteSpace: 'pre-wrap' }}>
            {zcReport.steps.map((s, i) => `${i + 1}. ${s}`).join('\n')}
            {zcReport.error && `\n错误：${zcReport.error}`}
          </div>
        </div>
      )}

      {/* ZCode 槽位卡片：随展开/收起动画整体收放（收起只留分组状态条与操作按钮）；插件停用整体置灰 */}
      <div className="grid grid-cols-2 gap-2.5" style={zcOff ? offStyle : undefined}>
        {zcSlots.length === 0 ? (
          <div
            className="card"
            style={{ padding: 18, color: 'var(--color-text-3)', fontSize: 12, gridColumn: '1/3' }}
          >
            还没有 ZCode 槽位 —— 登录 ZCode 后点「保存当前登录为槽位」。
          </div>
        ) : (
          <Collapse open={zcOpen}>{zcSlots.map((s) => zcCard(s))}</Collapse>
        )}
      </div>

      <div style={{ fontSize: 11, color: 'var(--color-text-3)', marginTop: 10 }}>
        · 安全红线：登录态只做文件级复制永不读内容 · WB 切换 检测进程 →（运行中需确认后关闭）→
        备份 → 恢复 → 校验 → 重启，失败自动回滚 · ZCode 切换 备份 → 原子写入 → JSON 校验，失败回滚，
        下次启动 ZCode 生效 · 关闭 WB 必须经用户确认，绝不静默强杀
      </div>

      {/* ═══ 对话框 ═══ */}
      {switchDlg && (
        <div
          style={{
            position: 'fixed', inset: 0, zIndex: 200,
            background: 'rgba(10,16,13,0.45)',
            display: 'flex', alignItems: 'center', justifyContent: 'center',
          }}
          onClick={() => !busy && setSwitchDlg(null)}
        >
          <div
            className="card"
            style={{ width: 'min(600px, 94vw)', maxHeight: '86vh', overflow: 'auto', padding: 18 }}
            onClick={(e) => e.stopPropagation()}
          >
            <b style={{ fontSize: 14 }}>⇄ 切换到「{switchDlg.label}」</b>
            <div style={{ fontSize: 11, color: 'var(--color-text-3)', margin: '3px 0 10px' }}>
              {data?.process_running
                ? 'WorkBuddy 正在运行，切换会先关闭它（未保存内容请先保存）'
                : 'WorkBuddy 将切换登录态并启动'}
            </div>

            {/* 运行中需勾选强制结束（保持「绝不静默强杀」红线） */}
            {data?.process_running && (
              <div
                className="flex items-center"
                style={{
                  gap: 8, marginBottom: 10, padding: '8px 12px', borderRadius: 10,
                  border: '1px solid color-mix(in srgb, var(--color-danger) 40%, transparent)',
                  background: 'color-mix(in srgb, var(--color-danger) 8%, transparent)',
                  fontSize: 11.5,
                }}
              >
                <span
                  className={killOn ? 'sw on' : 'sw'}
                  style={{ transform: 'scale(0.85)' }}
                  onClick={() => setKillOn((o) => !o)}
                />
                强制结束 WorkBuddy 并切换（当前未保存的工作将丢失）
              </div>
            )}


            <div className="flex items-center" style={{ gap: 8, marginTop: 14 }}>
              <span style={{ fontSize: 10.5, color: 'var(--color-warn)', flex: 1 }}>
                ⚠ 切换流程：关闭 WB → 备份 → 写入登录态 → 重启；失败自动回滚
              </span>
              <button className="btn gh" onClick={() => setSwitchDlg(null)} disabled={busy}>
                取消
              </button>
              <button
                className="btn p"
                disabled={busy || (data?.process_running && !killOn)}
                title={data?.process_running && !killOn ? '请先勾选「强制结束 WorkBuddy」' : undefined}
                onClick={() => {
                  const dlg = switchDlg;
                  setSwitchDlg(null);
                  if (dlg) void doSwitch(dlg.slotId, dlg.label, Boolean(data?.process_running && killOn));
                }}
              >
                开始切换
              </button>
            </div>
          </div>
        </div>
      )}
      {copyDlg && (
        <div
          style={{
            position: 'fixed', inset: 0, zIndex: 200,
            background: 'rgba(10,16,13,0.45)',
            display: 'flex', alignItems: 'center', justifyContent: 'center',
          }}
          onClick={() => !copyBusy && setCopyDlg(null)}
        >
          <div
            className="card"
            style={{ width: 'min(680px, 94vw)', maxHeight: '86vh', overflow: 'auto', padding: 18 }}
            onClick={(e) => e.stopPropagation()}
          >
            <b style={{ fontSize: 14 }}>⧉ 复制「{copyDlg.label}」的会话</b>
            <div style={{ fontSize: 11, color: 'var(--color-text-3)', margin: '3px 0 10px' }}>
              左侧勾选会话，右侧勾选目标账号（可多选，每个账号各复制一份）；新会话落在目标账号的同一项目下，源账号数据不变
            </div>

            {/* 复制要写 workbuddy.db —— WB 运行中绝不热写，需确认关闭（完成后自动重启还原现场） */}
            {data?.process_running && !copyReport?.ok && (
              <div
                className="flex items-center"
                style={{
                  gap: 8, marginBottom: 10, padding: '8px 12px', borderRadius: 10,
                  border: '1px solid color-mix(in srgb, var(--color-danger) 40%, transparent)',
                  background: 'color-mix(in srgb, var(--color-danger) 8%, transparent)',
                  fontSize: 11.5,
                }}
              >
                <span className={copyKillOn ? 'sw on' : 'sw'} style={{ transform: 'scale(0.85)' }} onClick={() => setCopyKillOn((o) => !o)} />
                强制结束 WorkBuddy 以写入会话（未保存内容将丢失；完成后自动重启）
              </div>
            )}

            <div style={{ display: 'grid', gridTemplateColumns: 'minmax(0, 1.35fr) minmax(0, 1fr)', gap: 12 }}>
              {/* 左：会话多选 */}
              <div>
                <div className="flex items-center" style={{ gap: 8, marginBottom: 6 }}>
                  <b style={{ fontSize: 11.5 }}>会话</b>
                  <span style={{ flex: 1 }} />
                  {sessList && sessList.some(selectable) && (
                    <button
                      className="btn gh"
                      style={{ padding: '2px 10px', fontSize: 10.5 }}
                      onClick={() =>
                        setSessSel((sel) =>
                          sel.length === sessList.filter(selectable).length
                            ? []
                            : sessList.filter(selectable).map((s) => s.id),
                        )
                      }
                    >
                      {sessSel.length === sessList.filter(selectable).length ? '取消全选' : '全选'}
                    </button>
                  )}
                </div>
                <input
                  value={sessSearch}
                  onChange={(e) => setSessSearch(e.target.value)}
                  placeholder="⌕ 搜索会话标题 / 项目…"
                  style={{
                    width: '100%', marginBottom: 8, padding: '5px 10px', fontSize: 11.5,
                    borderRadius: 8, border: '1px solid var(--color-border)',
                    background: 'var(--color-bg)', color: 'var(--color-text-1)', outline: 'none',
                  }}
                />
                <div style={{ border: '1px solid var(--color-border)', borderRadius: 10, overflow: 'hidden', maxHeight: 320, overflowY: 'auto' }}>
                  {sessLoading && (
                    <div style={{ padding: '14px 12px', fontSize: 11, color: 'var(--color-text-3)' }}>
                      <span className="spinning" style={{ display: 'inline-block' }}>↻</span> 正在读取会话…
                    </div>
                  )}
                  {!sessLoading && sessList && sessList.length === 0 && (
                    <div style={{ padding: '14px 12px', fontSize: 11, color: 'var(--color-text-3)' }}>
                      该账号没有可复制的会话
                    </div>
                  )}
                  {!sessLoading &&
                    sessList
                      ?.filter((s) => {
                        const q = sessSearch.trim().toLowerCase();
                        return !q || s.title.toLowerCase().includes(q) || s.project.toLowerCase().includes(q);
                      })
                      .slice(0, 50)
                      .map((s) => {
                        const checked = sessSel.includes(s.id);
                        const sel = selectable(s);
                        return (
                          <div
                            key={s.id}
                            className="flex items-center"
                            style={{
                              gap: 8, padding: '6px 10px', fontSize: 11.5,
                              borderTop: '1px solid var(--color-border)',
                              background: checked ? 'color-mix(in srgb, var(--color-primary) 7%, transparent)' : undefined,
                              opacity: sel ? 1 : 0.55,
                              cursor: sel ? 'pointer' : 'default',
                            }}
                            onClick={() => {
                              if (!sel) return;
                              setSessSel((prev) => (checked ? prev.filter((x) => x !== s.id) : [...prev, s.id]));
                            }}
                          >
                            <span className={checked && sel ? 'cbx on' : 'cbx'} style={{ marginTop: 0 }}>
                              {checked && sel ? '✓' : ''}
                            </span>
                            <div style={{ flex: 1, minWidth: 0 }}>
                              <div style={{ fontWeight: 700, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                                {s.custom_title || s.title || s.id.slice(0, 8)}
                              </div>
                              <div style={{ fontSize: 10, color: 'var(--color-text-3)', fontFamily: 'Consolas, monospace', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                                📁 {s.project} · {new Date(s.updated_at).toLocaleString('zh-CN', { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' })}
                              </div>
                            </div>
                            {s.claw ? (
                              <span className="chip err" title="Claw 工作区会话绑定账号渠道，不支持复制">不支持复制</span>
                            ) : !s.jsonl_ok ? (
                              <span className="chip warn" title="本机无会话正文，无法复制">正文缺失</span>
                            ) : s.linked ? (
                              <span className="chip ok" title="该会话此前已复制过副本">已关联</span>
                            ) : (
                              <span className="chip off">{s.status === 'completed' ? '已完成' : s.status === 'running' ? '进行中' : s.status || '—'}</span>
                            )}
                          </div>
                        );
                      })}
                </div>
              </div>

              {/* 右：目标账号多选 */}
              <div>
                <b style={{ fontSize: 11.5 }}>复制到</b>
                <div style={{ display: 'flex', flexDirection: 'column', gap: 6, marginTop: 6 }}>
                  {wbSlots.filter((x) => x.uid && x.uid !== copyDlg.uid).length === 0 && (
                    <div style={{ fontSize: 11, color: 'var(--color-text-3)', padding: 12, border: '1px dashed var(--color-border)', borderRadius: 10 }}>
                      暂无其他 WB 账号 —— 先保存或导入更多账号槽位
                    </div>
                  )}
                  {wbSlots
                    .filter((x) => x.uid && x.uid !== copyDlg.uid)
                    .map((t) => {
                      const on = copyTargets.includes(t.uid);
                      return (
                        <div
                          key={t.uid}
                          className="flex items-center"
                          style={{
                            gap: 8, padding: '8px 10px', borderRadius: 10, fontSize: 11.5,
                            border: `1px solid ${on ? 'var(--color-primary)' : 'var(--color-border)'}`,
                            background: on ? 'color-mix(in srgb, var(--color-primary) 6%, transparent)' : 'var(--color-surface)',
                            cursor: 'pointer',
                          }}
                          onClick={() => setCopyTargets((prev) => (on ? prev.filter((x) => x !== t.uid) : [...prev, t.uid]))}
                        >
                          <span className={on ? 'cbx on' : 'cbx'} style={{ marginTop: 0 }}>
                            {on ? '✓' : ''}
                          </span>
                          <div style={{ flex: 1, minWidth: 0 }}>
                            <div style={{ fontWeight: 700, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{t.label}</div>
                            <div style={{ fontSize: 10, color: 'var(--color-text-3)', fontFamily: 'Consolas, monospace' }}>
                              uid {t.uid.slice(0, 8)}…
                            </div>
                          </div>
                          {t.uid === data?.current?.uid && <span className="chip ok">当前生效</span>}
                        </div>
                      );
                    })}
                </div>
              </div>
            </div>

            {/* 复制结果报告 */}
            {copyReport && (
              <div
                style={{
                  marginTop: 12, padding: '10px 12px', borderRadius: 10, fontSize: 11.5,
                  border: `1px solid ${copyReport.ok ? 'var(--color-agent-wb)' : 'var(--color-danger)'}`,
                  background: copyReport.ok
                    ? 'color-mix(in srgb, var(--color-agent-wb) 6%, transparent)'
                    : 'color-mix(in srgb, var(--color-danger) 6%, transparent)',
                }}
              >
                <b style={{ color: copyReport.ok ? 'var(--color-agent-wb)' : 'var(--color-danger)' }}>
                  {copyReport.ok ? '✓ 复制完成' : copyReport.need_kill ? '⚠ 需要先关闭 WorkBuddy' : '✕ 复制未完成'}
                </b>
                {copyReport.killed && copyReport.restarted && (
                  <span style={{ color: 'var(--color-text-3)' }}>（WorkBuddy 已重新启动）</span>
                )}
                {copyReport.error && (
                  <div style={{ color: 'var(--color-text-2)', marginTop: 4 }}>{copyReport.error}</div>
                )}
                {copyReport.targets.length > 0 && (
                  <div style={{ marginTop: 6, display: 'flex', flexDirection: 'column', gap: 6 }}>
                    {copyReport.targets.map((g) => (
                      <div key={g.target_uid}>
                        <div style={{ fontWeight: 700 }}>→ {wbSlots.find((x) => x.uid === g.target_uid)?.label ?? g.target_uid.slice(0, 8)}</div>
                        {g.results.map((r) => (
                          <div key={r.source_id} className="flex items-center" style={{ gap: 6, marginTop: 2 }}>
                            <span>{r.status === 'copied' ? '✓' : r.status === 'skipped' ? '!' : '✕'}</span>
                            <span style={{ flex: 1, minWidth: 0, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                              {r.title || r.source_id.slice(0, 8)}
                            </span>
                            <span style={{ color: 'var(--color-text-3)', fontSize: 10 }}>
                              {r.detail || (r.status === 'copied' ? `新 ID ${r.new_id.slice(0, 8)}…` : '')}
                            </span>
                          </div>
                        ))}
                      </div>
                    ))}
                  </div>
                )}
              </div>
            )}

            <div className="flex items-center" style={{ gap: 8, marginTop: 14 }}>
              <span style={{ fontSize: 10.5, color: 'var(--color-warn)', flex: 1 }}>
                {copyReport?.ok
                  ? '切换到目标账号即可看到这些会话（WB 启动时自动补云端映射）'
                  : `将复制 ${sessSel.length} 个会话 → ${copyTargets.length} 个账号`}
              </span>
              <button className="btn gh" onClick={() => setCopyDlg(null)} disabled={copyBusy}>
                {copyReport?.ok ? '关闭' : '取消'}
              </button>
              {!copyReport?.ok && (
                <button
                  className="btn p"
                  onClick={() => void doCopySessions()}
                  disabled={copyBusy || (data?.process_running && !copyKillOn)}
                  title={data?.process_running && !copyKillOn ? '请先勾选「强制结束 WorkBuddy」' : undefined}
                >
                  {copyBusy ? (
                    <>
                      <span className="spinning" style={{ display: 'inline-block' }}>↻</span> 复制中…
                    </>
                  ) : (
                    `开始复制（${sessSel.length} 会话 → ${copyTargets.length} 账号）`
                  )}
                </button>
              )}
            </div>
          </div>
        </div>
      )}
      {confirm?.kind === 'delete' && (
        <ConfirmDialog
          title="删除槽位"
          message={`删除槽位「${confirm.label}」？\n槽位快照文件将一并删除（auth 登录态不受影响）。`}
          confirmLabel="删除"
          danger
          onConfirm={async () => {
            const c = confirm;
            setConfirm(null);
            try {
              await wbDeleteSlot(c.slotId);
              toast(`已删除槽位「${c.label}」`);
              await refresh();
            } catch (e) {
              setError(String(e));
            }
          }}
          onCancel={() => setConfirm(null)}
        />
      )}
      {naming && (
        <InputDialog
          title="保存 WB 槽位"
          message="为当前 WB 登录态起个名字，便于在槽位列表中识别。"
          fields={[{ key: 'label', label: '槽位名称', placeholder: '如：微信号 / 流量卡号' }]}
          confirmLabel="保存"
          onConfirm={(v) => {
            const label = v.label?.trim();
            setNaming(false);
            if (label) void doSaveCurrent(label);
          }}
          onCancel={() => setNaming(false)}
        />
      )}
      {editingBalance && (
        <InputDialog
          title={`账户积分 · ${editingBalance.label}`}
          message={
            editingBalance.balance != null
              ? `当前积分快照 ${Math.round(editingBalance.balance).toLocaleString()}。输入新数值即以该值为基准，后续用量与每日领取自动增减。`
              : '为此账户设置当前积分基准。此后每日用量与签到领取会自动增减，剩余 = 基准 − 设置后已用。'
          }
          fields={[
            {
              key: 'balance',
              label: '当前积分',
              defaultValue: editingBalance.balance != null ? String(Math.round(editingBalance.balance)) : '',
              placeholder: '如：3200（清空并输入 0 可清除该设置）',
            },
          ]}
          confirmLabel="保存"
          onConfirm={(v) => void saveBalance(editingBalance.uid, v.balance ?? '')}
          onCancel={() => setEditingBalance(null)}
        />
      )}
      {editingTotal && (
        <InputDialog
          title="每月积分总额度"
          message="填写 WB 账号的每月积分总额（如 10000）。剩余积分 = 总额度 − 本月已用（与官方账单同源）。填 0 清除。"
          fields={[{ key: 'total', label: '每月积分总额', placeholder: '如：10000' }]}
          confirmLabel="保存"
          onConfirm={(v) => void saveTotal(v)}
          onCancel={() => setEditingTotal(false)}
        />
      )}
      {zcConfirm && (
        <ConfirmDialog
          title={zcConfirm.kind === 'switch' ? '切换 ZCode 账号' : '删除 ZCode 槽位'}
          message={
            zcConfirm.kind === 'switch'
              ? `恢复「${zcConfirm.label}」的 ZCode 登录态？\n\n当前登录态会先备份，失败自动回滚。切换后下次启动 ZCode 生效（正在运行的 ZCode 会话不受影响）。`
              : `删除 ZCode 槽位「${zcConfirm.label}」？\n槽位快照文件将一并删除（当前登录态不受影响）。`
          }
          confirmLabel={zcConfirm.kind === 'switch' ? '恢复槽位' : '删除'}
          danger={zcConfirm.kind === 'delete'}
          onConfirm={async () => {
            const c = zcConfirm;
            setZcConfirm(null);
            if (!c) return;
            if (c.kind === 'switch') await zcDoSwitch(c.slotId, c.label);
            else {
              try {
                await zcodeDeleteSlot(c.slotId);
                toast(`已删除槽位「${c.label}」`);
                await refreshZc();
              } catch (e) {
                setError(String(e));
              }
            }
          }}
          onCancel={() => setZcConfirm(null)}
        />
      )}
      {zcNaming && (
        <InputDialog
          title="保存 ZCode 槽位"
          message="为当前 ZCode 登录态（~/.zcode/v2/credentials.json）起个名字。"
          fields={[{ key: 'label', label: '槽位名称', placeholder: '如：主号 / 小号' }]}
          confirmLabel="保存"
          onConfirm={(v) => {
            const label = v.label?.trim();
            setZcNaming(false);
            if (label) void zcDoSave(label);
          }}
          onCancel={() => setZcNaming(false)}
        />
      )}
    </div>
  );
}
