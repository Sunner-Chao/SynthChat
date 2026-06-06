import { useEffect, useRef, useState } from "react";
import {
  Check,
  ChevronRight,
  Container,
  Cpu,
  Download,
  ExternalLink,
  Globe,
  Loader2,
  Mic,
  Play,
  RefreshCw,
  Terminal,
  X,
  AlertTriangle,
  Zap,
} from "lucide-react";
import { api } from "../lib/api";

// Mock listen function for standalone frontend
function listen<T>(event: string, handler: (event: { payload: T }) => void): Promise<() => void> {
  console.log(`[Mock Event] Registered listener for: ${event}`);
  return Promise.resolve(() => {
    console.log(`[Mock Event] Unregistered listener for: ${event}`);
  });
}
import type { ActionResult, CheckItem, EnvCheckResult, InstallProgressEvent } from "../lib/types";

// ── Status Badge ──

function StatusBadge({ status }: { status: CheckItem["status"] }) {
  switch (status) {
    case "ok":
      return <span className="env-badge env-badge-ok"><Check size={14} /> 已就绪</span>;
    case "missing":
      return <span className="env-badge env-badge-missing"><X size={14} /> 未安装</span>;
    case "not_running":
      return <span className="env-badge env-badge-warn"><AlertTriangle size={14} /> 未运行</span>;
    case "installing":
      return <span className="env-badge env-badge-installing"><Loader2 size={14} className="env-spin" /> 安装中</span>;
    case "starting":
      return <span className="env-badge env-badge-starting"><Loader2 size={14} className="env-spin" /> 启动中</span>;
    case "error":
      return <span className="env-badge env-badge-error"><X size={14} /> 错误</span>;
    default:
      return null;
  }
}

// ── Icon for each check item ──

function ItemIcon({ id }: { id: string }) {
  const iconMap: Record<string, typeof Container> = {
    docker: Container,
    searxng: Globe,
    ollama: Cpu,
    vision_model: Cpu,
    python: Terminal,
    chattts: Mic,
    docker_requirements: Terminal,
  };
  const Icon = iconMap[id] ?? Zap;
  return <div className="env-item-icon"><Icon size={24} /></div>;
}

// ── Single Check Card ──

function CheckCard({
  item,
  progress,
  progressPercent,
  onFix,
  onDetails,
}: {
  item: CheckItem;
  progress?: string;
  progressPercent?: number;
  onFix: (action: string) => void;
  onDetails: (item: CheckItem) => void;
}) {
  const isWorking = item.status === "installing" || item.status === "starting" || !!progress;
  const summaryText = progress || item.detail.split(/\n/)[0];
  const hasMoreDetail = !progress && item.detail.includes("\n");
  const hasPercent = typeof progressPercent === "number" && Number.isFinite(progressPercent);
  const percent = hasPercent ? Math.max(0, Math.min(100, progressPercent)) : 0;

  return (
    <div className={`env-card env-card-${item.status}`}>
      <div className="env-card-header">
        <ItemIcon id={item.id} />
        <div className="env-card-info">
          <h3 className="env-card-title">{item.name}</h3>
          <p className="env-card-detail">{summaryText}</p>
        </div>
        <StatusBadge status={item.status} />
      </div>

      <div className={`env-progress-bar ${isWorking ? "active" : ""} ${hasPercent ? "determinate" : ""}`}>
        <div className="env-progress-fill" style={hasPercent ? { width: `${percent}%` } : undefined} />
      </div>

      <div className="env-card-action">
        {hasMoreDetail && (
          <button className="env-detail-btn" type="button" onClick={() => onDetails(item)}>
            <Terminal size={14} />
            查看详情
          </button>
        )}
        {item.status !== "ok" && !isWorking && item.fixAction && (
          <button className="env-fix-btn" onClick={() => onFix(item.fixAction!)}>
            {item.status === "not_running" ? <Play size={14} /> : <Download size={14} />}
            {item.fixLabel || "修复"}
          </button>
        )}
      </div>
    </div>
  );
}

// ── Main Environment Check Component ──

interface EnvironmentCheckProps {
  onComplete: () => void;
}

export function EnvironmentCheck({ onComplete }: EnvironmentCheckProps) {
  const [result, setResult] = useState<EnvCheckResult | null>(null);
  const [loading, setLoading] = useState(true);
  const [rechecking, setRechecking] = useState(false);
  const [progressMap, setProgressMap] = useState<Record<string, string>>({});
  const [progressPercentMap, setProgressPercentMap] = useState<Record<string, number>>({});
  const [actionResults, setActionResults] = useState<Record<string, ActionResult>>({});
  const [installingAll, setInstallingAll] = useState(false);
  const [installLogs, setInstallLogs] = useState<Array<{ time: string; id: string; stage: string; message: string }>>([]);
  const [detailItem, setDetailItem] = useState<CheckItem | null>(null);
  const completedRef = useRef<Set<string>>(new Set());

  // Run check — always wins over progress events
  const runCheck = async (silent = false) => {
    if (silent) {
      setRechecking(true);
    } else {
      setLoading(true);
    }
    try {
      const res = await api.checkEnvironment();
      // Mark all items from fresh check as "completed" so progress can't override them
      const freshIds = new Set<string>(res.items.map((i) => i.id));
      completedRef.current = freshIds;
      // Clear progress for all items that are now ok
      setProgressMap((prev) => {
        const next = { ...prev };
        for (const item of res.items) {
          if (item.status === "ok") delete next[item.id];
        }
        return next;
      });
      setProgressPercentMap((prev) => {
        const next = { ...prev };
        for (const item of res.items) {
          if (item.status === "ok") delete next[item.id];
        }
        return next;
      });
      setResult(res);
    } catch (e) {
      console.error("Environment check failed:", e);
    } finally {
      setLoading(false);
      setRechecking(false);
    }
  };

  useEffect(() => {
    runCheck(false);

    let unlisten: (() => void) | null = null;
    listen<InstallProgressEvent>("install-progress", (event) => {
      const { id, stage, message, percent } = event.payload;
      setInstallLogs((prev) => [
        ...prev.slice(-199),
        { time: new Date().toLocaleTimeString(), id, stage, message },
      ]);

      // If this item was just refreshed by runCheck, ignore stale progress events
      if (completedRef.current.has(id)) return;

      setProgressMap((prev) => ({ ...prev, [id]: message }));
      setProgressPercentMap((prev) => {
        if (typeof percent !== "number" || !Number.isFinite(percent)) return prev;
        return { ...prev, [id]: percent };
      });
      const isStarting = stage === "starting" || stage === "deploying";
      const newStatus: CheckItem["status"] = isStarting ? "starting" : "installing";
      setResult((prev) => {
        if (!prev) return prev;
        return {
          ...prev,
          items: prev.items.map((item) =>
            item.id === id ? { ...item, status: newStatus } : item
          ),
        };
      });
    }).then((fn) => {
      unlisten = fn;
    });

    return () => { unlisten?.(); };
  }, []);

  // Execute a fix action (with optional install path)
  const executeFix = async (action: string, installDir?: string) => {
    const actionMap: Record<string, (dir?: string) => Promise<ActionResult>> = {
      install_docker: (d) => api.installDocker(d),
      start_docker: () => api.startDockerDesktop(),
      setup_wsl2: () => api.setupWsl2(),
      install_ollama: (d) => api.installOllama(d),
      install_python: (d) => api.installPython(d),
      setup_searxng: () => api.setupSearxng(),
      start_ollama: () => api.startOllamaService(),
      pull_vision_model: () => api.pullVisionModel(),
      install_chattts_deps: (d) => api.installChatttsDeps(d),
    };

    const fn = actionMap[action];
    if (!fn) return;

    const item = result?.items.find((i) => i.fixAction === action);
    if (item) {
      completedRef.current.delete(item.id);
    }

    try {
      const res = await fn(installDir);
      setActionResults((prev) => ({ ...prev, [action]: res }));
    } catch (e) {
      console.error(`Action ${action} failed:`, e);
    } finally {
      if (item) {
        setProgressMap((prev) => {
          const next = { ...prev };
          delete next[item.id];
          return next;
        });
        setProgressPercentMap((prev) => {
          const next = { ...prev };
          delete next[item.id];
          return next;
        });
      }
    }

    setTimeout(() => runCheck(true), 500);
  };

  // Handle fix — open native folder picker for install actions, execute directly for others
  const handleFix = async (action: string) => {
    if (action === "install_docker") {
      const folder = await api.pickFolder("选择 Docker Desktop 安装目录");
      executeFix(action, folder || undefined);
    } else if (action === "install_ollama") {
      const folder = await api.pickFolder("选择 Ollama 安装目录");
      executeFix(action, folder || undefined);
    } else if (action === "install_python") {
      const folder = await api.pickFolder("选择 Python 运行时安装目录");
      executeFix(action, folder || undefined);
    } else if (action === "install_chattts_deps") {
      const folder = await api.pickFolder("选择 ChatTTS 模型存放目录");
      executeFix(action, folder || undefined);
    } else {
      executeFix(action);
    }
  };

  // Install all missing
  const handleInstallAll = async () => {
    setInstallingAll(true);
    try {
      await api.installAllMissing();
    } catch (e) {
      console.error("Install all failed:", e);
    }
    setInstallingAll(false);
    setTimeout(() => runCheck(true), 500);
  };

  const handleCancel = async () => {
    try {
      await api.cancelEnvironmentAction();
    } catch (e) {
      console.error("Cancel environment action failed:", e);
    }
    setInstallingAll(false);
    setProgressMap({});
    setProgressPercentMap({});
    setTimeout(() => runCheck(true), 800);
  };

  const allPassed = result?.allPassed ?? false;
  const hasMissing = result?.items.some((i) => i.status !== "ok") ?? false;
  const isAnyInstalling =
    installingAll ||
    result?.items.some((i) => i.status === "installing" || i.status === "starting") ||
    Object.keys(progressMap).length > 0;

  return (
    <div className="env-root">
      <div className="env-bg-orb env-bg-orb-1" />
      <div className="env-bg-orb env-bg-orb-2" />
      <div className="env-bg-orb env-bg-orb-3" />

      <div className="env-container">
        <div className="env-header">
          <div className="env-logo">
            <div className="env-logo-icon"><Zap size={32} /></div>
            <h1 className="env-title">SynthChat</h1>
          </div>
          <p className="env-subtitle">环境配置检查</p>
          <p className="env-desc">正在检查您的系统环境，确保所有依赖服务已就绪</p>
        </div>

        {loading && !result && (
          <div className="env-loading">
            <Loader2 size={32} className="env-spin" />
            <p>正在检测环境...</p>
          </div>
        )}

        {result && (
          <>
            <div className={`env-summary ${allPassed ? "env-summary-ok" : "env-summary-warn"}`}>
              <div className="env-summary-icon">
                {allPassed ? <Check size={20} /> : <AlertTriangle size={20} />}
              </div>
              <div className="env-summary-text">
                {allPassed
                  ? "所有环境检查已通过，可以正常使用 SynthChat 的全部功能"
                  : "部分环境依赖未就绪，以下功能可能受限"}
              </div>
              <button
                className="env-recheck-btn"
                onClick={() => runCheck(true)}
                disabled={isAnyInstalling || rechecking}
              >
                {rechecking ? <Loader2 size={14} className="env-spin" /> : <RefreshCw size={14} />}
                {rechecking ? "检测中..." : "重新检测"}
              </button>
            </div>

            <div className="env-grid">
              {result.items.map((item) => (
                <CheckCard
                  key={item.id}
                  item={item}
                  progress={progressMap[item.id]}
                  progressPercent={progressPercentMap[item.id]}
                  onFix={handleFix}
                  onDetails={setDetailItem}
                />
              ))}
            </div>

            {Object.entries(actionResults).map(([action, res]) => (
              <div key={action} className={`env-action-result ${res.success ? "env-action-ok" : "env-action-fail"}`}>
                <strong>{res.message}</strong>
                {res.detail && <p>{res.detail}</p>}
              </div>
            ))}

            {installLogs.length > 0 && (
              <div className="env-log-panel">
                <div className="env-log-header">
                  <span><Terminal size={14} /> 下载/安装日志</span>
                  <button type="button" onClick={() => setInstallLogs([])}>清空</button>
                </div>
                <div className="env-log-body">
                  {installLogs.map((log, index) => (
                    <div className="env-log-line" key={`${log.time}-${index}`}>
                      <span className="env-log-time">{log.time}</span>
                      <span className="env-log-tag">{log.id}/{log.stage}</span>
                      <span className="env-log-message">{log.message}</span>
                    </div>
                  ))}
                </div>
              </div>
            )}

            {detailItem && (
              <div className="env-dialog-backdrop" onClick={() => setDetailItem(null)}>
                <div className="env-dialog" onClick={(event) => event.stopPropagation()}>
                  <div className="env-dialog-header">
                    <strong>{detailItem.name}详情</strong>
                    <button type="button" onClick={() => setDetailItem(null)}>关闭</button>
                  </div>
                  <pre className="env-dialog-body">{detailItem.detail}</pre>
                </div>
              </div>
            )}

            <div className="env-actions">
              {isAnyInstalling && (
                <button className="env-btn env-btn-danger" onClick={handleCancel}>
                  <X size={16} /> 取消当前任务
                </button>
              )}
              {hasMissing && !isAnyInstalling && (
                <button className="env-btn env-btn-secondary" onClick={handleInstallAll}>
                  <Zap size={16} /> 一键安装全部缺失依赖
                </button>
              )}
              <button className="env-btn env-btn-primary" onClick={onComplete}>
                {allPassed ? <><ChevronRight size={16} /> 进入应用</> : <><ExternalLink size={16} /> 跳过检查，直接进入</>}
              </button>
            </div>
          </>
        )}
      </div>

      <style>{`
        .env-root { position: fixed; inset: 0; z-index: 9999; background: var(--bg, #F8FAFC); overflow-y: auto; font-family: var(--font, "Outfit", sans-serif); }
        .env-bg-orb { position: fixed; border-radius: 50%; filter: blur(80px); opacity: 0.15; pointer-events: none; }
        .env-bg-orb-1 { width: 400px; height: 400px; background: var(--primary, #0891B2); top: -100px; right: -100px; }
        .env-bg-orb-2 { width: 300px; height: 300px; background: var(--accent, #06B6D4); bottom: -50px; left: -50px; }
        .env-bg-orb-3 { width: 200px; height: 200px; background: var(--primary-light, #E0F7FA); top: 50%; left: 50%; transform: translate(-50%, -50%); }
        .env-container { position: relative; max-width: 880px; margin: 0 auto; padding: 48px 24px; }
        .env-header { text-align: center; margin-bottom: 36px; }
        .env-logo { display: flex; align-items: center; justify-content: center; gap: 12px; margin-bottom: 8px; }
        .env-logo-icon { width: 56px; height: 56px; border-radius: 16px; background: linear-gradient(135deg, var(--primary, #0891B2), var(--accent, #06B6D4)); display: flex; align-items: center; justify-content: center; color: white; box-shadow: 0 8px 24px rgba(8, 145, 178, 0.3); }
        .env-title { font-size: 32px; font-weight: 700; color: var(--text-1, #0F172A); margin: 0; }
        .env-subtitle { font-size: 18px; font-weight: 500; color: var(--text-2, #475569); margin: 4px 0 0; }
        .env-desc { font-size: 14px; color: var(--text-3, #94A3B8); margin: 8px 0 0; }
        .env-loading { display: flex; flex-direction: column; align-items: center; gap: 12px; padding: 48px; color: var(--text-2, #475569); }
        .env-summary { display: flex; align-items: center; gap: 12px; padding: 14px 20px; border-radius: var(--radius-md, 12px); margin-bottom: 24px; backdrop-filter: blur(12px); }
        .env-summary-ok { background: rgba(16, 185, 129, 0.08); border: 1px solid rgba(16, 185, 129, 0.2); color: #065F46; }
        .env-summary-warn { background: rgba(245, 158, 11, 0.08); border: 1px solid rgba(245, 158, 11, 0.2); color: #92400E; }
        .env-summary-icon { flex-shrink: 0; }
        .env-summary-text { flex: 1; font-size: 14px; font-weight: 500; }
        .env-recheck-btn { display: inline-flex; align-items: center; gap: 6px; padding: 6px 14px; border: 1px solid var(--divider, rgba(71, 85, 105, 0.1)); border-radius: var(--radius-sm, 8px); background: var(--card, #FFFFFF); color: var(--text-2, #475569); font-size: 13px; cursor: pointer; transition: all 0.2s; white-space: nowrap; }
        .env-recheck-btn:hover:not(:disabled) { border-color: var(--primary, #0891B2); color: var(--primary, #0891B2); }
        .env-recheck-btn:disabled { opacity: 0.6; cursor: not-allowed; }
        .env-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 16px; margin-bottom: 24px; }
        @media (max-width: 640px) { .env-grid { grid-template-columns: 1fr; } }
        .env-card { background: var(--card, #FFFFFF); border: 1px solid var(--divider, rgba(71, 85, 105, 0.1)); border-radius: var(--radius-md, 12px); padding: 16px; min-height: 130px; display: flex; flex-direction: column; min-width: 0; overflow: hidden; box-shadow: var(--shadow-card, 0 1px 3px rgba(0,0,0,0.04)); transition: border-color 0.25s, box-shadow 0.25s; }
        .env-card:hover { box-shadow: var(--shadow-elevated, 0 8px 32px rgba(0,0,0,0.08)); }
        .env-card-ok { border-left: 3px solid var(--success, #10B981); }
        .env-card-missing { border-left: 3px solid var(--danger, #EF4444); }
        .env-card-not_running { border-left: 3px solid var(--warning, #F59E0B); }
        .env-card-installing { border-left: 3px solid var(--primary, #0891B2); }
        .env-card-starting { border-left: 3px solid var(--primary, #0891B2); }
        .env-card-error { border-left: 3px solid var(--danger, #EF4444); }
        .env-card-header { display: flex; align-items: flex-start; gap: 12px; }
        .env-item-icon { width: 44px; height: 44px; border-radius: var(--radius-sm, 8px); background: var(--surface-2, #F1F5F9); display: flex; align-items: center; justify-content: center; color: var(--primary, #0891B2); flex-shrink: 0; }
        .env-card-info { flex: 1; min-width: 0; }
        .env-card-title { font-size: 15px; font-weight: 600; color: var(--text-1, #0F172A); margin: 0 0 4px; }
        .env-card-detail { font-size: 12px; color: var(--text-3, #94A3B8); margin: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
        .env-badge { display: inline-flex; align-items: center; gap: 4px; padding: 3px 10px; border-radius: var(--radius-pill, 999px); font-size: 12px; font-weight: 600; white-space: nowrap; flex-shrink: 0; }
        .env-badge-ok { background: rgba(16, 185, 129, 0.1); color: #065F46; }
        .env-badge-missing { background: rgba(239, 68, 68, 0.1); color: #991B1B; }
        .env-badge-warn { background: rgba(245, 158, 11, 0.1); color: #92400E; }
        .env-badge-installing { background: rgba(8, 145, 178, 0.1); color: #0E7490; }
        .env-badge-starting { background: rgba(8, 145, 178, 0.1); color: #0E7490; }
        .env-badge-error { background: rgba(239, 68, 68, 0.1); color: #991B1B; }
        .env-progress-bar { margin-top: 12px; height: 4px; border-radius: 2px; overflow: hidden; visibility: hidden; }
        .env-progress-bar.active { visibility: visible; }
        .env-progress-fill { height: 100%; background: linear-gradient(90deg, var(--primary, #0891B2), var(--accent, #06B6D4)); border-radius: 2px; width: 0%; }
        .env-progress-bar.active .env-progress-fill { animation: env-progress-sweep 1.5s ease-in-out infinite; }
        .env-progress-bar.determinate { background: rgba(8, 145, 178, 0.14); }
        .env-progress-bar.determinate .env-progress-fill { animation: none; margin-left: 0; transition: width 0.25s ease; }
        @keyframes env-progress-sweep { 0% { width: 0%; margin-left: 0; } 50% { width: 60%; margin-left: 20%; } 100% { width: 0%; margin-left: 100%; } }
        .env-card-action { margin-top: 12px; min-height: 30px; display: flex; align-items: flex-start; gap: 8px; flex-wrap: wrap; }
        .env-detail-btn { display: inline-flex; align-items: center; gap: 6px; padding: 6px 12px; border: 1px solid var(--divider, rgba(71, 85, 105, 0.1)); border-radius: var(--radius-sm, 8px); background: var(--surface-2, #F1F5F9); color: var(--text-2, #475569); font-size: 13px; font-weight: 600; cursor: pointer; transition: border-color 0.2s, color 0.2s; }
        .env-detail-btn:hover { border-color: var(--primary, #0891B2); color: var(--primary, #0891B2); }
        .env-fix-btn { display: inline-flex; align-items: center; gap: 6px; padding: 6px 14px; border: none; border-radius: var(--radius-sm, 8px); background: linear-gradient(135deg, var(--primary, #0891B2), var(--accent, #06B6D4)); color: white; font-size: 13px; font-weight: 600; cursor: pointer; transition: box-shadow 0.2s, transform 0.2s; }
        .env-fix-btn:hover { box-shadow: 0 4px 12px rgba(8, 145, 178, 0.3); transform: translateY(-1px); }
        .env-action-result { padding: 12px 16px; border-radius: var(--radius-sm, 8px); margin-bottom: 12px; font-size: 13px; }
        .env-action-result p { margin: 4px 0 0; font-size: 12px; opacity: 0.8; word-break: break-all; }
        .env-action-ok { background: rgba(16, 185, 129, 0.08); border: 1px solid rgba(16, 185, 129, 0.2); color: #065F46; }
        .env-action-fail { background: rgba(239, 68, 68, 0.08); border: 1px solid rgba(239, 68, 68, 0.2); color: #991B1B; }
        .env-log-panel { background: var(--card, #FFFFFF); border: 1px solid var(--divider, rgba(71, 85, 105, 0.1)); border-radius: var(--radius-sm, 8px); margin: 12px 0 20px; overflow: hidden; }
        .env-log-header { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 10px 12px; border-bottom: 1px solid var(--divider, rgba(71, 85, 105, 0.1)); color: var(--text-2, #475569); font-size: 13px; font-weight: 700; }
        .env-log-header span { display: inline-flex; align-items: center; gap: 6px; }
        .env-log-header button { border: 1px solid var(--divider, rgba(71, 85, 105, 0.1)); background: var(--surface-2, #F1F5F9); color: var(--text-2, #475569); border-radius: 6px; padding: 4px 8px; font-size: 12px; cursor: pointer; }
        .env-log-body { max-height: 380px; min-height: 180px; overflow: auto; padding: 10px 12px; background: #0F172A; color: #E2E8F0; font-family: ui-monospace, SFMono-Regular, Consolas, "Liberation Mono", monospace; font-size: 12px; line-height: 1.6; }
        .env-log-line { display: grid; grid-template-columns: 78px 140px minmax(0, 1fr); gap: 8px; align-items: baseline; }
        .env-log-time { color: #94A3B8; }
        .env-log-tag { color: #67E8F9; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
        .env-log-message { overflow-wrap: anywhere; }
        .env-dialog-backdrop { position: fixed; inset: 0; z-index: 10000; background: rgba(15, 23, 42, 0.45); display: flex; align-items: center; justify-content: center; padding: 24px; }
        .env-dialog { width: min(760px, 100%); max-height: min(720px, 88vh); background: var(--card, #FFFFFF); border-radius: var(--radius-sm, 8px); box-shadow: 0 24px 80px rgba(15, 23, 42, 0.28); display: flex; flex-direction: column; overflow: hidden; }
        .env-dialog-header { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 14px 16px; border-bottom: 1px solid var(--divider, rgba(71, 85, 105, 0.1)); color: var(--text-1, #0F172A); }
        .env-dialog-header button { border: 1px solid var(--divider, rgba(71, 85, 105, 0.1)); background: var(--surface-2, #F1F5F9); color: var(--text-2, #475569); border-radius: 6px; padding: 5px 10px; font-size: 13px; cursor: pointer; }
        .env-dialog-body { margin: 0; padding: 14px 16px; overflow: auto; white-space: pre-wrap; overflow-wrap: anywhere; color: var(--text-2, #475569); font-family: ui-monospace, SFMono-Regular, Consolas, "Liberation Mono", monospace; font-size: 12px; line-height: 1.65; }
        .env-actions { display: flex; align-items: center; justify-content: center; gap: 16px; margin-top: 8px; }
        .env-btn { display: inline-flex; align-items: center; gap: 8px; padding: 12px 24px; border: none; border-radius: var(--radius-md, 12px); font-size: 15px; font-weight: 600; cursor: pointer; transition: all 0.25s cubic-bezier(0.16, 1, 0.3, 1); font-family: var(--font, "Outfit", sans-serif); }
        .env-btn:disabled { opacity: 0.5; cursor: not-allowed; }
        .env-btn-primary { background: linear-gradient(135deg, var(--primary, #0891B2), var(--primary-dark, #0E7490)); color: white; box-shadow: 0 4px 16px rgba(8, 145, 178, 0.3); }
        .env-btn-primary:hover:not(:disabled) { box-shadow: 0 8px 24px rgba(8, 145, 178, 0.4); transform: translateY(-2px); }
        .env-btn-secondary { background: var(--card, #FFFFFF); color: var(--text-1, #0F172A); border: 1px solid var(--divider-strong, rgba(71, 85, 105, 0.16)); }
        .env-btn-secondary:hover:not(:disabled) { border-color: var(--primary, #0891B2); color: var(--primary, #0891B2); }
        .env-btn-danger { background: rgba(239, 68, 68, 0.1); color: #991B1B; border: 1px solid rgba(239, 68, 68, 0.28); }
        .env-btn-danger:hover:not(:disabled) { background: rgba(239, 68, 68, 0.16); transform: translateY(-2px); }
        .env-spin { animation: env-spin 1s linear infinite; }
        @keyframes env-spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }
      `}</style>
    </div>
  );
}
