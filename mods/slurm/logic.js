const isRunning = (state) => state === "RUNNING" || state === "COMPLETING";

export function endedJobs(prevRunning, snapshot) {
  if (!snapshot || snapshot.error) return { running: prevRunning, ended: [] };
  const machines = {};
  for (const m of snapshot.machines || []) machines[m.name] = m;
  const running = {};
  const ended = [];
  if (prevRunning) {
    for (const key of Object.keys(prevRunning)) {
      const entry = prevRunning[key];
      const m = machines[entry.machine];
      if (!m || m.error) running[key] = entry;
    }
  }
  for (const m of snapshot.machines || []) {
    if (m.error) continue;
    for (const j of m.jobs || []) {
      if (isRunning(j.state)) running[`${m.name}/${j.id}`] = { machine: m.name, id: j.id, name: j.name };
    }
  }
  if (prevRunning) {
    for (const key of Object.keys(prevRunning)) {
      if (!(key in running)) ended.push(prevRunning[key]);
    }
  }
  return { running, ended };
}

const ERROR_STATES = ["FAILED", "OUT_OF_MEMORY", "TIMEOUT", "NODE_FAIL", "CANCELLED", "PREEMPTED", "BOOT_FAIL", "DEADLINE"];

export function notifyLevel(state) {
  const token = String(state || "").trim().split(/\s+/)[0].toUpperCase();
  if (token === "COMPLETED") return "success";
  if (ERROR_STATES.includes(token)) return "error";
  return "warning";
}

export function gpuUnderuse(allocGpus, gpus) {
  if (!(allocGpus > 0) || !gpus || gpus.length === 0) return null;
  const busy = gpus.filter((g) => g.util >= 10).length;
  return busy < allocGpus ? `${busy} of ${allocGpus} GPUs busy` : null;
}

export function storagePct(entry) {
  return Math.max(entry.pct, entry.quota?.pct ?? 0);
}

export function storageColorKey(pct) {
  if (pct >= 90) return "blocked";
  if (pct >= 80) return "ansi3";
  return "ansi2";
}

export function storageAlerts(prevArmed, cluster) {
  const armed = { ...(prevArmed || {}) };
  const alerts = [];
  for (const m of cluster?.machines || []) {
    if (m.error) continue;
    for (const s of m.storage || []) {
      const key = `${m.name}:${s.path}`;
      const pct = storagePct(s);
      const isArmed = armed[key] !== false;
      if (isArmed && pct >= 90) {
        alerts.push({ machine: m.name, path: s.path, pct });
        armed[key] = false;
      } else if (!isArmed && pct < 85) {
        armed[key] = true;
      } else {
        armed[key] = isArmed;
      }
    }
  }
  return { armed, alerts };
}

const DAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const pad2 = (n) => (n < 10 ? "0" : "") + n;

export function formatStart(iso, now) {
  if (!iso) return "";
  const m = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})(?::(\d{2}))?$/.exec(String(iso).trim());
  if (!m) return "";
  const d = new Date(+m[1], +m[2] - 1, +m[3], +m[4], +m[5], +(m[6] || 0));
  if (isNaN(d.getTime())) return "";
  const hm = `${pad2(d.getHours())}:${pad2(d.getMinutes())}`;
  const sameDay = d.getFullYear() === now.getFullYear() && d.getMonth() === now.getMonth() && d.getDate() === now.getDate();
  return sameDay ? hm : `${DAYS[d.getDay()]} ${hm}`;
}

export function shouldLead(lease, me, now, isWatcher) {
  if (!lease) return true;
  if (lease.copy === me) return true;
  if (now - lease.at > 60000) return true;
  return Boolean(isWatcher && !lease.watch);
}
