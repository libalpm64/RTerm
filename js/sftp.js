import {
  sessions, activeId, sftpSshId, sftpInitialized, localExpanded,
  setSftpSshId, setSftpInitialized
} from './state.js';

let sftpSelectMode = false;
const sftpSelected = new Map(); // path -> { dir, name }
let sftpModalResolve = null;
let sftpPickerResolve = null;
let sftpPrevBodyOverflow = '';
let sftpPrevHtmlOverflow = '';

function setSftpPageScrollLock(locked) {
  if (locked) {
    sftpPrevBodyOverflow = document.body.style.overflow || '';
    sftpPrevHtmlOverflow = document.documentElement.style.overflow || '';
    document.body.style.overflow = 'hidden';
    document.documentElement.style.overflow = 'hidden';
  } else {
    document.body.style.overflow = sftpPrevBodyOverflow;
    document.documentElement.style.overflow = sftpPrevHtmlOverflow;
  }
}

function ensureSftpActionModal() {
  if (document.getElementById('sftp-action-modal')) return;
  const modal = document.createElement('div');
  modal.id = 'sftp-action-modal';
  modal.style.cssText = 'position:fixed;inset:0;background:rgba(0,0,0,.6);backdrop-filter:blur(2px);display:none;align-items:center;justify-content:center;z-index:8000;';
  modal.innerHTML = `<div style="width:560px;max-width:92vw;background:var(--bg2);border:1px solid var(--border2);border-radius:8px;">
    <div id="sftp-action-title" style="padding:12px 16px;border-bottom:1px solid var(--border);font-size:14px;font-weight:600;color:var(--text);">Action</div>
    <div id="sftp-action-body" style="padding:12px 16px;color:var(--text2);font-size:12px;line-height:1.55;"></div>
    <div id="sftp-action-input-wrap" style="display:none;padding:0 16px 12px 16px;"><input id="sftp-action-input" type="text" autocomplete="off" spellcheck="false" style="width:100%;background:var(--bg3);border:1px solid var(--border2);border-radius:6px;padding:8px;color:var(--text);font-size:12px;"></div>
    <div style="display:flex;justify-content:flex-end;gap:8px;padding:10px 16px;border-top:1px solid var(--border);"><button id="sftp-action-cancel" class="btn btn-cancel">Cancel</button><button id="sftp-action-ok" class="btn btn-primary">OK</button></div>
  </div>`;
  document.body.appendChild(modal);
  const close = () => { modal.style.display = 'none'; if (sftpModalResolve) { sftpModalResolve(null); sftpModalResolve = null; } };
  document.getElementById('sftp-action-cancel').onclick = close;
  modal.addEventListener('click', (e) => { if (e.target === modal) close(); });
}

function openSftpActionModal({ title, body, withInput = false, value = '', okText = 'OK', showCancel = true }) {
  ensureSftpActionModal();
  const modal = document.getElementById('sftp-action-modal');
  const t = document.getElementById('sftp-action-title');
  const b = document.getElementById('sftp-action-body');
  const w = document.getElementById('sftp-action-input-wrap');
  const i = document.getElementById('sftp-action-input');
  const ok = document.getElementById('sftp-action-ok');
  const cancel = document.getElementById('sftp-action-cancel');
  t.textContent = title;
  b.innerHTML = body;
  w.style.display = withInput ? 'block' : 'none';
  i.value = value || '';
  ok.textContent = okText;
  cancel.style.display = showCancel ? '' : 'none';
  modal.style.display = 'flex';
  if (withInput) setTimeout(() => i.focus(), 0);
  if (withInput) {
    b.querySelectorAll('.sftp-dir-pick').forEach((el) => {
      el.addEventListener('click', () => {
        i.value = el.getAttribute('data-dir') || getCurrentSftpPath() || '/';
      });
    });
  }
  return new Promise((resolve) => {
    sftpModalResolve = resolve;
    ok.onclick = () => {
      const out = withInput ? i.value.trim() : true;
      modal.style.display = 'none';
      sftpModalResolve = null;
      resolve(out);
    };
  });
}

async function fetchRemotePermString(fullPath) {
  const sshId = sftpSshId ?? Array.from(sessions.values()).find(s => s.type === 'ssh' && s.sshId !== null && s.sshId !== undefined)?.sshId;
  if (sshId === null || sshId === undefined) return 'unknown';
  const cmd = `stat -c %A ${shellQuote(fullPath)} 2>/dev/null || stat -f %Sp ${shellQuote(fullPath)} 2>/dev/null || ls -ld ${shellQuote(fullPath)} 2>/dev/null | awk '{print $1}' || echo unknown`;
  const res = await window.rterm.sshExec(sshId, cmd);
  if (!res?.success) return 'unknown';
  const tok = String(res.result || '').trim().split(/\s+/)[0] || 'unknown';
  return tok.length >= 9 ? tok : 'unknown';
}

async function showRemoteInfo(file, fullPath, sshId) {
  const perms = await fetchRemotePermString(fullPath);
  let contentsHtml = '';
  if (file.dir) {
    const listing = await window.rterm.sftpList(sshId, fullPath);
    if (listing?.success) {
      const children = Array.isArray(listing.result) ? listing.result : [];
      const folders = children.filter(child => child.dir).length;
      const files = children.length - folders;
      const preview = children.slice(0, 120).map(child =>
        `<div style="display:flex;align-items:center;gap:6px;padding:2px 0;min-width:0;">` +
        `<span style="color:${child.dir ? 'var(--accent)' : 'var(--text3)'};font-size:11px;">${child.dir ? 'DIR' : 'FILE'}</span>` +
        `<span style="overflow:hidden;text-overflow:ellipsis;white-space:nowrap;">${escapeHtml(child.name)}</span></div>`
      ).join('');
      contentsHtml = `<br><b>Contains:</b> ${files} file${files === 1 ? '' : 's'}, ${folders} folder${folders === 1 ? '' : 's'}` +
        `<div style="margin-top:6px;max-height:150px;overflow:auto;padding:6px 8px;background:var(--bg3);border:1px solid var(--border);border-radius:4px;color:var(--text2);">${preview || '<span style="color:var(--text3)">(empty folder)</span>'}${children.length > 120 ? `<div style="padding-top:4px;color:var(--text3);">and ${children.length - 120} more...</div>` : ''}</div>`;
    } else {
      contentsHtml = '<br><span style="color:var(--text3)">Contents unavailable</span>';
    }
  }
  openSftpActionModal({
    title: file.dir ? 'Remote Folder Info' : 'Remote File Info',
    body: `Location: SFTP<br>Name: ${escapeHtml(file.name)}<br>Type: ${file.dir ? 'Folder' : 'File'}<br>Permissions: ${escapeHtml(perms)}<br>Size: ${file.size || 0} bytes<br>Path: ${escapeHtml(fullPath)}${contentsHtml}`,
    okText: 'Close',
    showCancel: false
  });
}

function permToRwx(perm) {
  const n = Number(perm);
  if (!Number.isFinite(n) || n === 0) return 'unknown';
  const v = n & 0o777;
  const tri = (x) => `${x & 4 ? 'r' : '-'}${x & 2 ? 'w' : '-'}${x & 1 ? 'x' : '-'}`;
  return tri((v >> 6) & 7) + tri((v >> 3) & 7) + tri(v & 7);
}

function currentSftpSessionId() {
  return sftpSshId ?? Array.from(sessions.values()).find(s => s.type === 'ssh' && s.sshId !== null && s.sshId !== undefined)?.sshId;
}

function normalizeRemotePath(value) {
  const path = String(value || '').trim();
  if (!path) return '/';
  return path.replace(/[\\/]+/g, '/').replace(/\/\.\//g, '/');
}

function parentLocalPath(value) {
  const path = String(value || '').replace(/\\/g, '/').replace(/\/+$/, '');
  if (!path || path === '/' || path === '~') return '/';
  if (path.startsWith('~/')) {
    const parts = path.slice(2).split('/').filter(Boolean);
    return parts.length > 1 ? `~/${parts.slice(0, -1).join('/')}` : '~';
  }
  const driveMatch = path.match(/^([A-Za-z]:)(?:\/(.*))?$/);
  if (driveMatch) {
    const parts = (driveMatch[2] || '').split('/').filter(Boolean);
    return parts.length > 1 ? `${driveMatch[1]}/${parts.slice(0, -1).join('/')}` : `${driveMatch[1]}/`;
  }
  const parts = path.split('/').filter(Boolean);
  if (path.startsWith('/')) return '/' + parts.slice(0, -1).join('/');
  return parts.slice(0, -1).join('/') || '/';
}

function renderSftpPathEditor(pathEl, path) {
  const value = normalizeRemotePath(path);
  // Use the path field for navigation
  pathEl.innerHTML = `<input id="sftp-path-input" aria-label="Remote directory" value="${escapeHtml(value)}" placeholder="/remote/path" style="flex:1;min-width:0;height:20px;padding:1px 6px;border:1px solid var(--border2);border-radius:3px;background:var(--bg2);color:var(--text);font:11px 'JetBrains Mono',monospace;outline:none;">`;
  const input = document.getElementById('sftp-path-input');
  const submit = () => {
    const next = normalizeRemotePath(input?.value);
    if (next !== _currentSftpPath) loadSftpDir(next);
    else if (input) input.value = next;
  };
  input?.addEventListener('keydown', (event) => {
    if (event.key === 'Enter') {
      event.preventDefault();
      submit();
    }
  });
  input?.addEventListener('change', submit);
}

const MAX_REMOTE_DOWNLOAD_FILES = 10000;
const DEFAULT_SFTP_CONCURRENCY = 3;
const MAX_SFTP_CONCURRENCY = 10;

function configuredSftpConcurrency() {
  const value = Number.parseInt(document.getElementById('setting-sftp-concurrent')?.value, 10);
  if (!Number.isFinite(value)) return DEFAULT_SFTP_CONCURRENCY;
  return Math.max(1, Math.min(MAX_SFTP_CONCURRENCY, value));
}

/** Run async work with bounded concurrency */
async function runWithConcurrency(items, worker, limit = configuredSftpConcurrency()) {
  if (!items.length) return;
  let next = 0;
  const workerCount = Math.min(limit, items.length);
  const runners = Array.from({ length: workerCount }, async () => {
    while (true) {
      const index = next++;
      if (index >= items.length) return;
      try {
        await worker(items[index], index);
      } catch (error) {
        // Continue remaining transfers after a file error
        console.error('SFTP transfer failed:', error);
      }
    }
  });
  await Promise.all(runners);
}

async function runWithConcurrencyIterable(iterable, worker, limit = configuredSftpConcurrency()) {
  const iterator = iterable[Symbol.asyncIterator]();
  const workerCount = Math.max(1, limit);
  let iteratorError = null;
  const runners = Array.from({ length: workerCount }, async () => {
    while (!iteratorError) {
      let step;
      try {
        step = await iterator.next();
      } catch (error) {
        iteratorError = error;
        return;
      }
      if (step.done) return;
      try {
        await worker(step.value);
      } catch (error) {
        console.error('SFTP transfer failed:', error);
      }
    }
  });
  await Promise.all(runners);
  if (iteratorError) throw iteratorError;
}

function joinRemotePath(base, name) {
  const root = normalizeRemotePath(base);
  return root === '/' ? `/${name}` : `${root}/${name}`;
}

function joinLocalPath(base, relative) {
  const left = String(base || '').replace(/\\/g, '/').replace(/\/+$/, '');
  const right = String(relative || '').replace(/\\/g, '/').replace(/^\/+/, '');
  if (!left) return right;
  if (!right) return left;
  return `${left}/${right}`;
}

/** Stream remote files with bounded concurrency */
async function* iterateRemoteFiles(sshId, remotePath, relativePath = '', state = { count: 0 }, visited = new Set()) {
  const path = normalizeRemotePath(remotePath);
  if (visited.has(path)) return;
  visited.add(path);
  const listing = await window.rterm.sftpList(sshId, path);
  if (!listing?.success) throw new Error(listing?.error || `Unable to list ${path}`);

  for (const child of (Array.isArray(listing.result) ? listing.result : [])) {
    if (!child?.name || child.name === '.' || child.name === '..') continue;
    const childPath = joinRemotePath(path, child.name);
    const childRelative = relativePath ? `${relativePath}/${child.name}` : child.name;
    if (child.dir) {
      yield* iterateRemoteFiles(sshId, childPath, childRelative, state, visited);
      continue;
    }
    state.count += 1;
    if (state.count > MAX_REMOTE_DOWNLOAD_FILES) {
      throw new Error(`Folder contains more than ${MAX_REMOTE_DOWNLOAD_FILES.toLocaleString()} files`);
    }
    yield { path: childPath, name: child.name, relative: childRelative };
  }
}

async function downloadRemoteFolder(sshId, remotePath, folderName, saveDir) {
  const transferId = `folder-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 7)}`;
  window.__rterm_transferProgress?.(transferId, `Scanning ${folderName}...`, 0);
  try {
    const state = { count: 0 };
    const files = iterateRemoteFiles(sshId, remotePath, '', state);
    const destinationRoot = joinLocalPath(saveDir, folderName);
    await runWithConcurrencyIterable(files, async (file) => {
      const relativeDir = file.relative.includes('/')
        ? file.relative.slice(0, file.relative.lastIndexOf('/'))
        : '';
      const localDir = joinLocalPath(destinationRoot, relativeDir);
      await window.rterm.sftpDownload(sshId, file.path, file.name, localDir);
    });
    if (state.count === 0) {
      window.__rterm_transferProgress?.(transferId, `${folderName} is empty`, 100, { done: true });
      return;
    }
    window.__rterm_transferProgress?.(transferId, `Downloaded ${state.count} file${state.count === 1 ? '' : 's'}`, 100, { done: true });
  } catch (error) {
    window.__rterm_transferProgress?.(transferId, `Folder download failed: ${error?.message || error}`, 100, { error: true });
  }
}

function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}

function buildMoveFolderList(files, currentPath) {
  const dirs = (files || []).filter(x => x.dir).map(x => x.name).slice(0, 200);
  const up = currentPath !== '/' ? `<div class="sftp-dir-pick" data-dir=".." style="padding:5px 8px;border-radius:4px;cursor:pointer;color:var(--text2);">..</div>` : '';
  if (!dirs.length && !up) return '<div style="margin-top:8px;color:var(--text3)">(no subfolders)</div>';
  return `<div style="margin-top:8px;max-height:220px;overflow:auto;border:1px solid var(--border2);border-radius:6px;padding:4px;background:var(--bg3);">${up}${dirs.map(d => {
    const target = (currentPath === '/' ? '' : currentPath) + '/' + d;
    return `<div class="sftp-dir-pick" data-dir="${escapeHtml(target)}" style="padding:5px 8px;border-radius:4px;cursor:pointer;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;color:var(--text2);">${escapeHtml(d)}</div>`;
  }).join('')}</div>`;
}

async function pickSftpTargetDir(initialPath, title, itemName) {
  if (!document.getElementById('sftp-move-picker')) {
    const m = document.createElement('div');
    m.id = 'sftp-move-picker';
    m.style.cssText = 'position:fixed;inset:0;background:rgba(0,0,0,.62);backdrop-filter:blur(2px);display:none;align-items:center;justify-content:center;z-index:8100;';
    m.innerHTML = `<div style="width:680px;max-width:94vw;height:520px;max-height:86vh;background:var(--bg2);border:1px solid var(--border2);border-radius:8px;display:flex;flex-direction:column;overflow:hidden;">
      <div style="padding:12px 14px;border-bottom:1px solid var(--border);font-size:14px;font-weight:600;color:var(--text);" id="sftp-move-title"></div>
      <div style="padding:8px 14px;color:var(--text3);font-family:monospace;font-size:11px;border-bottom:1px solid var(--border);" id="sftp-move-path"></div>
      <div id="sftp-move-list" style="flex:1;min-height:0;overflow-y:auto;overflow-x:hidden;padding:8px 10px;scrollbar-width:none;"></div>
      <div style="display:flex;gap:8px;justify-content:flex-end;padding:10px 14px;border-top:1px solid var(--border);">
        <div style="display:flex;gap:8px;">
          <button id="sftp-move-cancel" class="btn btn-cancel">Cancel</button>
          <button id="sftp-move-here" class="btn btn-primary">Move Here</button>
        </div>
      </div>
    </div>`;
    document.body.appendChild(m);
    m.addEventListener('click', (e) => {
      if (e.target === m && sftpPickerResolve) { sftpPickerResolve(null); sftpPickerResolve = null; m.style.display = 'none'; }
    });
  }
  const modal = document.getElementById('sftp-move-picker');
  const titleEl = document.getElementById('sftp-move-title');
  const pathEl = document.getElementById('sftp-move-path');
  const listEl = document.getElementById('sftp-move-list');
  const cancelBtn = document.getElementById('sftp-move-cancel');
  const hereBtn = document.getElementById('sftp-move-here');
  let path = initialPath || '/';
  let selectedDir = null;
  const render = async () => {
    titleEl.textContent = `${title}: ${itemName}`;
    pathEl.textContent = path;
    listEl.innerHTML = '<div style="padding:8px;color:var(--text3)">Loading...</div>';
    const sshId = currentSftpSessionId();
    if (sshId === null || sshId === undefined) { listEl.innerHTML = '<div style="padding:8px;color:var(--red)">No SFTP session</div>'; return; }
    const res = await window.rterm.sftpList(sshId, path);
    if (!res?.success) { listEl.innerHTML = `<div style="padding:8px;color:var(--red)">${escapeHtml(res?.error || 'list failed')}</div>`; return; }
    const dirs = (res.result || []).filter(x => x.dir);
    selectedDir = null;
    listEl.innerHTML = '';
    if (path !== '/') {
      const up = document.createElement('div');
      up.style.cssText = 'display:flex;align-items:center;gap:8px;padding:6px 8px;border-radius:6px;cursor:pointer;color:var(--text2);';
      up.innerHTML = `<span style="width:14px;height:14px;display:inline-flex;"><svg viewBox="0 0 24 24" fill="none" stroke="var(--text2)" stroke-width="1.6"><path d="M3 12h18"></path><path d="M9 6l-6 6 6 6"></path></svg></span><span>.../</span>`;
      up.onclick = async () => { path = '/' + path.split('/').filter(Boolean).slice(0, -1).join('/'); if (path === '') path = '/'; await render(); };
      listEl.appendChild(up);
    }
    if (!dirs.length && path === '/') { listEl.innerHTML = '<div style="padding:8px;color:var(--text3)">(empty)</div>'; return; }
    for (const d of dirs) {
      const full = (path === '/' ? '' : path) + '/' + d.name;
      const row = document.createElement('div');
      row.style.cssText = 'display:flex;align-items:center;gap:8px;padding:6px 8px;border-radius:6px;cursor:pointer;color:var(--text2);';
      row.innerHTML = `<span style="width:14px;height:14px;display:inline-flex;"><svg viewBox="0 0 24 24" fill="none" stroke="var(--accent)" stroke-width="1.6"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"></path></svg></span><span style="overflow:hidden;text-overflow:ellipsis;white-space:nowrap;">${escapeHtml(d.name)}</span>`;
      row.onmouseenter = () => { if (selectedDir !== full) row.style.background = 'var(--bg4)'; };
      row.onmouseleave = () => { if (selectedDir !== full) row.style.background = 'transparent'; };
      row.onclick = () => {
        selectedDir = full;
        Array.from(listEl.children).forEach(c => { c.style.background = 'transparent'; c.style.color = 'var(--text2)'; });
        row.style.background = 'rgba(79,195,247,.18)';
        row.style.color = 'var(--text)';
      };
      row.ondblclick = async () => { path = full; await render(); };
      listEl.appendChild(row);
    }
  };
  await render();
  modal.style.display = 'flex';
  setSftpPageScrollLock(true);
  return await new Promise((resolve) => {
    sftpPickerResolve = resolve;
    cancelBtn.onclick = () => { modal.style.display = 'none'; setSftpPageScrollLock(false); sftpPickerResolve = null; resolve(null); };
    hereBtn.onclick = () => { const out = selectedDir || path; modal.style.display = 'none'; setSftpPageScrollLock(false); sftpPickerResolve = null; resolve(out); };
  });
}

const sftpOpenedIds = new Set();

export async function initSftp(id) {
  if (sftpOpenedIds.has(id)) return true;
  const result = await window.rterm.sftpOpen(id);
  if (result.success) {
    sftpOpenedIds.add(id);
    setSftpInitialized(true);
    return true;
  }
  console.error('SFTP open failed:', result.error);
  return false;
}

// Release per session state when SSH closes
export function forgetSftpSession(id) {
  sftpOpenedIds.delete(id);
  if (sftpSshId === id) {
    setSftpSshId(null);
    setSftpInitialized(false);
    sftpSelected.clear();
  }
}

export async function loadSftpDir(path) {
  path = normalizeRemotePath(path);
  _currentSftpPath = path;
  const listEl = document.getElementById('sftp-list');
  const pathEl = document.getElementById('sftp-path');
  const statusEl = document.getElementById('sftp-status');
  if (!listEl || !statusEl || !pathEl) return;

  const activeSsh = activeId ? sessions.get(activeId) : null;
  const sess = (activeSsh?.type === 'ssh' && activeSsh.sshId !== null && activeSsh.sshId !== undefined)
    ? activeSsh : Array.from(sessions.values()).find(s => s.type === 'ssh' && s.sshId !== null && s.sshId !== undefined);

  if (!sess || sess.sshId === null || sess.sshId === undefined) {
    listEl.innerHTML = '<div style="padding:8px;color:var(--text3)">No active SSH session</div>';
    return;
  }

  setSftpSshId(sess.sshId);
  statusEl.classList.remove('connected');
  statusEl.classList.add('connecting');
  statusEl.querySelector('.label').textContent = 'Connecting...';
  renderSftpPathEditor(pathEl, path);

  const ok = await initSftp(sftpSshId);
  if (!ok) {
    statusEl.classList.remove('connecting');
    listEl.innerHTML = '<div style="padding:8px;color:var(--red)">SFTP initialization failed</div>';
    return;
  }

  statusEl.classList.remove('connecting');
  statusEl.classList.add('connected');
  statusEl.querySelector('.label').textContent = 'Connected';

  listEl.innerHTML = '<div style="padding:8px;color:var(--text3)">Loading...</div>';

  try {
    const result = await window.rterm.sftpList(sftpSshId, path);
    if (!result.success) {
      listEl.innerHTML = '<div style="color:var(--red);padding:8px">Error: ' + (result.error || 'unknown') + '</div>';
      renderSftpPathEditor(pathEl, path);
      return;
    }

    listEl.innerHTML = '';
    renderSftpPathEditor(pathEl, path);
    ensureSftpBulkBar();
    renderSftpBulkBar();
    let currentPath = path;

    if (path !== '/') {
      const parentDiv = document.createElement('div');
      parentDiv.className = 'filer-item';
      parentDiv.innerHTML = '<span class="icon"><svg viewBox="0 0 24 24" fill="none" stroke="var(--text3)" stroke-width="1.5"><path d="M22 19a2 2 0 01-2 2H4a2 2 0 01-2-2V5a2 2 0 012-2h5l2 3h9a2 2 0 012 2z"></path></svg></span><span class="name" style="color:var(--text3)">..</span>';
      parentDiv.onclick = () => {
        const parent = '/' + path.split('/').filter(Boolean).slice(0, -1).join('/');
        loadSftpDir(parent === '' ? '/' : parent);
      };
      listEl.appendChild(parentDiv);
    }

    const files = result.result || [];
    for (const f of files) {
      const div = document.createElement('div');
      div.className = 'filer-item';
      const icon = f.dir
        ? '<svg viewBox="0 0 24 24" fill="none" stroke="var(--accent)" stroke-width="1.5"><path d="M22 19a2 2 0 01-2 2H4a2 2 0 01-2-2V5a2 2 0 012-2h5l2 3h9a2 2 0 012 2z"></path></svg>'
        : '<svg viewBox="0 0 24 24" fill="none" stroke="var(--text2)" stroke-width="1.5"><path d="M14 2H6a2 2 0 00-2 2v16a2 2 0 002 2h12a2 2 0 002-2V8z"></path><polyline points="14 2 14 8 20 8"></polyline></svg>';
      const sizeStr = f.size ? (f.size > 1024 * 1024 * 1024 ? (f.size / 1024 / 1024 / 1024).toFixed(1) + 'G' : f.size > 1024 * 1024 ? (f.size / 1024 / 1024).toFixed(1) + 'M' : f.size > 1024 ? (f.size / 1024).toFixed(1) + 'K' : f.size + 'B') : '';
      const fullPath = (path === '/' ? '/' : path + '/') + f.name;
      div.innerHTML = `<span class="icon">${icon}</span><span class="name">${escapeHtml(f.name)}</span>${sizeStr ? `<span class="size">${sizeStr}</span>` : ''}`;
      if (f.dir) {
        div.onclick = () => {
          if (sftpSelectMode) return toggleSftpSelect(div, fullPath, f);
          loadSftpDir(fullPath);
        };
        div.oncontextmenu = (e) => {
          e.preventDefault();
          e.stopPropagation();
          const old = document.getElementById('sftp-ctx');
          if (old) old.remove();
          const menu = document.createElement('div');
          menu.id = 'sftp-ctx';
          menu.style.cssText = 'position:fixed;z-index:9999;background:var(--bg3);border:1px solid var(--border2);border-radius:6px;padding:4px;box-shadow:0 8px 24px rgba(0,0,0,.5);min-width:160px;font-size:12px;';
          menu.innerHTML = '<div class="dropdown-item" id="sctx-dl">Download folder...</div><div class="dropdown-item" id="sctx-mv">Move...</div><div class="dropdown-item" id="sctx-rn">Rename...</div><div class="dropdown-item" id="sctx-info">Info</div><div class="dropdown-item" id="sctx-sm">Select Mode</div>';
          document.body.appendChild(menu);
          menu.addEventListener('mousedown', (ev) => ev.stopPropagation());
          menu.addEventListener('click', (ev) => ev.stopPropagation());
          menu.style.left = Math.max(4, Math.min(e.clientX, window.innerWidth - 188)) + 'px';
          menu.style.top = Math.max(4, Math.min(e.clientY, window.innerHeight - menu.offsetHeight - 4)) + 'px';
          document.getElementById('sctx-dl').onclick = async () => {
            menu.remove();
            const sshId = currentSftpSessionId();
            if (sshId === null || sshId === undefined) { alert('No active SSH session'); return; }
            const saveDir = await showSaveDialog(f.name);
            if (!saveDir) return;
            void downloadRemoteFolder(sshId, fullPath, f.name, saveDir);
          };
          document.getElementById('sctx-mv').onclick = async () => {
            menu.remove();
            const targetDir = await pickSftpTargetDir(getCurrentSftpPath() || '/', 'Move Remote Item', f.name);
            if (!targetDir) return;
            const sshId = currentSftpSessionId();
            if (sshId === null || sshId === undefined) return;
            const toPath = (targetDir === '/' ? '' : targetDir) + '/' + f.name;
            const mv = await window.rterm.sftpRename(sshId, fullPath, toPath);
            if (!mv?.success) {
              await openSftpActionModal({ title: 'Move Failed', body: String(mv?.error || 'unknown error'), okText: 'Close', showCancel: false });
              return;
            }
            loadSftpDir(getCurrentSftpPath() || '/');
          };
          document.getElementById('sctx-rn').onclick = async () => {
            menu.remove();
            const newName = await openSftpActionModal({ title: 'Rename Remote Item', body: `Rename <b>${escapeHtml(f.name)}</b> to:`, withInput: true, value: f.name, okText: 'Rename' });
            if (!newName || newName === f.name) return;
            const baseDir = fullPath.split('/').slice(0, -1).join('/') || '/';
            const toPath = (baseDir === '/' ? '' : baseDir) + '/' + newName;
            const sshId = currentSftpSessionId();
            if (sshId === null || sshId === undefined) return;
            const mv = await window.rterm.sftpRename(sshId, fullPath, toPath);
            if (!mv?.success) {
              await openSftpActionModal({ title: 'Rename Failed', body: String(mv?.error || 'unknown error'), okText: 'Close', showCancel: false });
              return;
            }
            loadSftpDir(getCurrentSftpPath() || '/');
          };
          document.getElementById('sctx-info').onclick = async () => {
            menu.remove();
            await showRemoteInfo(f, fullPath, currentSftpSessionId());
          };
          document.getElementById('sctx-sm').onclick = () => {
            menu.remove();
            setSftpSelectMode(true);
            renderSftpBulkBar();
          };
          requestAnimationFrame(() => {
            const closeMenu = (ev) => {
              const m = document.getElementById('sftp-ctx');
              if (!m) return;
              if (m.contains(ev.target)) return;
              m.remove();
              document.removeEventListener('pointerdown', closeMenu, true);
            };
            document.addEventListener('pointerdown', closeMenu, true);
          });
        };
      } else {
        div.addEventListener('click', (e) => {
          if (sftpSelectMode) {
            return toggleSftpSelect(div, fullPath, f);
          }
        });
        div.oncontextmenu = (e) => {
          e.preventDefault();
          e.stopPropagation();
          const old = document.getElementById('sftp-ctx');
          if (old) old.remove();
          const menu = document.createElement('div');
          menu.id = 'sftp-ctx';
          menu.style.cssText = 'position:fixed;z-index:9999;background:var(--bg3);border:1px solid var(--border2);border-radius:6px;padding:4px;box-shadow:0 8px 24px rgba(0,0,0,.5);min-width:160px;font-size:12px;';
          menu.innerHTML = '<div class="dropdown-item" id="sctx-dl">Download to...</div><div class="dropdown-item" id="sctx-mv">Move...</div><div class="dropdown-item" id="sctx-rn">Rename...</div><div class="dropdown-item" id="sctx-info">Info</div><div class="dropdown-item" id="sctx-sm">Select Mode</div>';
          document.body.appendChild(menu);
          menu.addEventListener('mousedown', (ev) => ev.stopPropagation());
          menu.addEventListener('click', (ev) => ev.stopPropagation());
          menu.style.left = Math.max(4, Math.min(e.clientX, window.innerWidth - 188)) + 'px';
          menu.style.top = Math.max(4, Math.min(e.clientY, window.innerHeight - 88)) + 'px';
          document.getElementById('sctx-dl').onclick = async () => {
            menu.remove();
            const saveDir = await showSaveDialog(f.name);
            if (!saveDir) return;
            void window.rterm.sftpDownload(sftpSshId, fullPath, f.name, saveDir);
          };
          document.getElementById('sctx-mv').onclick = async () => {
            menu.remove();
            const targetDir = await pickSftpTargetDir(getCurrentSftpPath() || '/', 'Move Remote Item', f.name);
            if (!targetDir) return;
            const sshId = currentSftpSessionId();
            if (sshId === null || sshId === undefined) return;
            const toPath = (targetDir === '/' ? '' : targetDir) + '/' + f.name;
            const mv = await window.rterm.sftpRename(sshId, fullPath, toPath);
            if (!mv?.success) {
              await openSftpActionModal({ title: 'Move Failed', body: String(mv?.error || 'unknown error'), okText: 'Close', showCancel: false });
              return;
            }
            loadSftpDir(getCurrentSftpPath() || '/');
          };
          document.getElementById('sctx-rn').onclick = async () => {
            menu.remove();
            const newName = await openSftpActionModal({ title: 'Rename Remote Item', body: `Rename <b>${escapeHtml(f.name)}</b> to:`, withInput: true, value: f.name, okText: 'Rename' });
            if (!newName || newName === f.name) return;
            const baseDir = fullPath.split('/').slice(0, -1).join('/') || '/';
            const toPath = (baseDir === '/' ? '' : baseDir) + '/' + newName;
            const sshId = currentSftpSessionId();
            if (sshId === null || sshId === undefined) return;
            const mv = await window.rterm.sftpRename(sshId, fullPath, toPath);
            if (!mv?.success) {
              await openSftpActionModal({ title: 'Rename Failed', body: String(mv?.error || 'unknown error'), okText: 'Close', showCancel: false });
              return;
            }
            loadSftpDir(getCurrentSftpPath() || '/');
          };
          document.getElementById('sctx-info').onclick = async () => {
            menu.remove();
            await showRemoteInfo(f, fullPath, currentSftpSessionId());
          };
          document.getElementById('sctx-sm').onclick = () => {
            menu.remove();
            setSftpSelectMode(true);
            renderSftpBulkBar();
          };
          requestAnimationFrame(() => {
            const closeMenu = (ev) => {
              const m = document.getElementById('sftp-ctx');
              if (!m) return;
              if (m.contains(ev.target)) return;
              m.remove();
              document.removeEventListener('pointerdown', closeMenu, true);
            };
            document.addEventListener('pointerdown', closeMenu, true);
          });
        };
      }
      if (sftpSelected.has(fullPath)) div.classList.add('selected');
      listEl.appendChild(div);
    }
  } catch (e) {
    listEl.innerHTML = '<div style="color:var(--red);padding:8px">Error: ' + e + '</div>';
  }
}

let _sdResolve = null;
let _sdPath = '~';
let _sdLastFolder = '~';
let _sdFilename = '';
let _sdMode = 'folder';
let _sdLoadToken = 0;
let _currentSftpPath = '/';

export function getCurrentSftpPath() {
  return _currentSftpPath;
}

export function showSaveDialog(filename, mode = 'folder') {
  return new Promise((resolve) => {
    _sdResolve = resolve;
    _sdFilename = filename;
    _sdMode = mode;
    // Keep picker path at a valid directory
    _sdPath = localExpanded || _sdLastFolder || '~';
    document.getElementById('save-dialog').style.display = 'flex';
    const header = document.getElementById('sd-header-title');
    if (header) header.textContent = mode === 'file' ? 'Select Local File to Upload' : 'Select Download Folder';
    const selectButton = document.getElementById('sd-select');
    if (selectButton) selectButton.textContent = mode === 'file' ? 'Select File' : 'Select Folder';
    loadSaveDir(filename, _sdPath);
  });
}

function loadSaveDir(filename, dir) {
  const list = document.getElementById('sd-list');
  const pathEl = document.getElementById('sd-path');
  if (!list || !pathEl) return;
  const loadToken = ++_sdLoadToken;
  _sdPath = dir;
  _sdLastFolder = dir;
  pathEl.textContent = dir === '~' ? 'Home' : dir;
  list.innerHTML = '<div style="padding:16px;color:var(--text3);text-align:center;font-size:13px;">Loading...</div>';

  window.rterm.localList(dir).then(result => {
    if (loadToken !== _sdLoadToken) return;
    if (!result || !result.success) {
      pathEl.textContent = dir === '~' ? (window.__rterm_home || 'Home') : dir;
      list.innerHTML = '<div style="padding:16px;color:var(--red);text-align:center;font-size:13px;">' + escapeHtml(result?.error || 'Cannot read directory') + '</div>';
      return;
    }
    if (result.home) window.__rterm_home = result.home;
    pathEl.textContent = result.path || (dir === '~' ? (result.home || 'Home') : dir);
    const files = result.result || [];
    list.innerHTML = '';

    if (dir !== '/' && dir !== '~') {
      const p = document.createElement('div');
      p.className = 'sd-item';
      p.innerHTML = '<span class="icon"><svg viewBox="0 0 24 24" fill="none" stroke="var(--text3)" stroke-width="1.5"><path d="M22 19a2 2 0 01-2 2H4a2 2 0 01-2-2V5a2 2 0 012-2h5l2 3h9a2 2 0 012 2z"></path></svg></span><span>..</span>';
      p.onclick = () => {
        _sdPath = parentLocalPath(dir);
        loadSaveDir(filename, _sdPath);
      };
      list.appendChild(p);
    }

    let hasItems = false;
    for (const f of files) {
      if (!f.dir && _sdMode === 'folder') continue;
      hasItems = true;
      const d = document.createElement('div');
      d.className = 'sd-item';
      const icon = f.dir
        ? '<svg viewBox="0 0 24 24" fill="none" stroke="var(--accent)" stroke-width="1.5"><path d="M22 19a2 2 0 01-2 2H4a2 2 0 01-2-2V5a2 2 0 012-2h5l2 3h9a2 2 0 012 2z"></path></svg>'
        : '<svg viewBox="0 0 24 24" fill="none" stroke="var(--text2)" stroke-width="1.5"><path d="M14 2H6a2 2 0 00-2 2v16a2 2 0 002 2h12a2 2 0 002-2V8z"></path><polyline points="14 2 14 8 20 8"></polyline></svg>';
      d.innerHTML = `<span class="icon">${icon}</span><span>${escapeHtml(f.name)}</span>`;
      const full = (dir === '/' ? '' : dir === '~' ? '~' : dir) + '/' + f.name;
      if (f.dir) {
        d.onclick = () => { _sdPath = full; loadSaveDir(filename, full); };
        d.ondblclick = () => { _sdPath = full; finishSaveDialog(filename); };
      } else {
        d.onclick = () => {
          _sdPath = full;
          list.querySelectorAll('.sd-item').forEach(x => x.classList.remove('selected'));
          d.classList.add('selected');
        };
        d.ondblclick = () => { _sdPath = full; finishSaveDialog(filename); };
      }
      list.appendChild(d);
    }
    if (!hasItems && dir !== '/') {
      list.innerHTML += '<div style="padding:16px;color:var(--text3);text-align:center;font-size:12px;">(empty directory)</div>';
    }
  }).catch(error => {
    if (loadToken !== _sdLoadToken) return;
    // Keep picker usable when directory IPC fails
    pathEl.textContent = dir === '~' ? (window.__rterm_home || 'Home') : dir;
    list.innerHTML = '<div style="padding:16px;color:var(--red);text-align:center;font-size:13px;">' + escapeHtml(error?.message || error || 'Cannot read directory') + '</div>';
  });
}

function finishSaveDialog(filename) {
  let path = _sdPath;
  if (path === '~') {
    // Preserve home token for cross platform path expansion
    path = window.__rterm_home || '~';
  } else if (_sdMode === 'file') {
    document.getElementById('save-dialog').style.display = 'none';
    if (_sdResolve) { _sdResolve(path); _sdResolve = null; }
    return;
  }
  document.getElementById('save-dialog').style.display = 'none';
  if (_sdResolve) { _sdResolve(path); _sdResolve = null; }
}

export function setupSaveDialog() {
  document.getElementById('sd-close').onclick = () => {
    _sdLoadToken += 1;
    document.getElementById('save-dialog').style.display = 'none';
    if (_sdResolve) { _sdResolve(null); _sdResolve = null; }
  };
  document.getElementById('sd-cancel').onclick = () => {
    _sdLoadToken += 1;
    document.getElementById('save-dialog').style.display = 'none';
    if (_sdResolve) { _sdResolve(null); _sdResolve = null; }
  };
  document.getElementById('sd-select').onclick = () => {
    if (_sdMode === 'file') {
      const selected = document.querySelector('#sd-list .sd-item.selected');
      if (!selected) return;
    }
    finishSaveDialog(_sdFilename);
  };
}

window.loadSftpDir = loadSftpDir;

function toggleSftpSelect(el, path, fileMeta) {
  if (sftpSelected.has(path)) {
    sftpSelected.delete(path);
    el.classList.remove('selected');
    el.style.background = '';
    el.style.color = '';
  } else {
    sftpSelected.set(path, { dir: !!fileMeta.dir, name: fileMeta.name });
    el.classList.add('selected');
    if (sftpSelectMode) {
      el.style.background = 'rgba(255,36,55,.18)';
      el.style.color = 'var(--red)';
    }
  }
  renderSftpBulkBar();
}

function ensureSftpBulkBar() {
  if (document.getElementById('sftp-bulk-bar')) return;
  const container = document.getElementById('panel-sftp');
  const bar = document.createElement('div');
  bar.id = 'sftp-bulk-bar';
  bar.style.cssText = 'display:none;gap:8px;padding:6px 8px;border-top:1px solid var(--border);background:var(--bg3);align-items:center;font-size:11px;';
  bar.innerHTML = `<span id="sftp-bulk-count" style="color:var(--text2)"></span><button id="sftp-bulk-exit" class="btn btn-cancel" style="padding:4px 8px;margin-left:auto;">Cancel</button>`;
  container.appendChild(bar);
  ensureSftpTopActions();

  document.getElementById('sftp-bulk-exit').onclick = () => {
    setSftpSelectMode(false);
  };

}

function ensureSftpTopActions() {
  if (document.getElementById('sftp-bulk-download-top')) return;
  const icons = document.querySelector('#panel-sftp .panel-icons');
  if (!icons) return;
  const dl = document.createElement('span');
  dl.id = 'sftp-bulk-download-top';
  dl.className = 'panel-icon';
  dl.title = 'Download Selected';
  dl.style.cssText = 'display:none;';
  dl.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" y1="15" x2="12" y2="3"/></svg>';
  const mv = document.createElement('span');
  mv.id = 'sftp-bulk-move-top';
  mv.className = 'panel-icon';
  mv.title = 'Move Selected';
  mv.style.cssText = 'display:none;';
  mv.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="5 9 2 12 5 15"/><polyline points="9 5 12 2 15 5"/><polyline points="15 19 12 22 9 19"/><polyline points="19 9 22 12 19 15"/><line x1="2" y1="12" x2="22" y2="12"/><line x1="12" y1="2" x2="12" y2="22"/></svg>';
  const del = document.createElement('span');
  del.id = 'sftp-bulk-delete-top';
  del.className = 'panel-icon';
  del.title = 'Delete Selected';
  del.style.cssText = 'display:none;color:var(--red);';
  del.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="3 6 5 6 21 6"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6"/><path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/></svg>';
  icons.insertBefore(del, document.getElementById('sftp-refresh'));
  icons.insertBefore(mv, del);
  icons.insertBefore(dl, mv);
  dl.onclick = async () => {
    const entries = Array.from(sftpSelected.entries()).filter(([, v]) => !v.dir);
    void runWithConcurrency(entries, async ([p, meta]) => {
      await window.rterm.sftpDownload(sftpSshId, p, meta.name);
    });
  };
  mv.onclick = async () => {
    const targetDir = await openSftpActionModal({ title: 'Move Selected Items', body: `Move ${sftpSelected.size} selected item(s) to directory:`, withInput: true, value: getCurrentSftpPath() || '/', okText: 'Move' });
    if (!targetDir) return;
    const sshId = currentSftpSessionId();
    if (sshId === null || sshId === undefined) return;
    for (const [p, meta] of Array.from(sftpSelected.entries())) {
      await window.rterm.sftpRename(sshId, p, (targetDir === '/' ? '' : targetDir) + '/' + meta.name);
    }
    sftpSelected.clear();
    renderSftpBulkBar();
    loadSftpDir(getCurrentSftpPath() || '/');
  };

  del.onclick = async () => {
    const sess = sessions.get(activeId);
    if (!sess?.sshId) return;
    for (const [p] of Array.from(sftpSelected.entries())) {
      await window.rterm.sshExec(sess.sshId, `rm -rf ${shellQuote(p)}`);
    }
    sftpSelected.clear();
    renderSftpBulkBar();
    loadSftpDir(getCurrentSftpPath() || '/');
  };
}

function renderSftpBulkBar() {
  const bar = document.getElementById('sftp-bulk-bar');
  const count = document.getElementById('sftp-bulk-count');
  if (!bar || !count) return;
  bar.style.display = sftpSelectMode ? 'flex' : 'none';
  count.textContent = `${sftpSelected.size} selected`;
  const dl = document.getElementById('sftp-bulk-download-top');
  const mv = document.getElementById('sftp-bulk-move-top');
  const del = document.getElementById('sftp-bulk-delete-top');
  if (dl) dl.style.display = sftpSelectMode ? '' : 'none';
  if (mv) mv.style.display = sftpSelectMode ? '' : 'none';
  if (del) del.style.display = sftpSelectMode ? '' : 'none';
  const cancelBtn = document.getElementById('sftp-bulk-exit');
  if (cancelBtn) {
    cancelBtn.style.boxShadow = sftpSelectMode ? '0 0 0 1px rgba(79,195,247,.45), 0 0 10px rgba(79,195,247,.35)' : '';
  }
}

export function setSftpSelectMode(enabled) {
  sftpSelectMode = enabled;
  if (!enabled) {
    sftpSelected.clear();
    document.querySelectorAll('#sftp-list .filer-item.selected').forEach(el => {
      el.classList.remove('selected');
      el.style.background = '';
      el.style.color = '';
    });
  }
  renderSftpBulkBar();
}

function shellQuote(s) {
  return `'${String(s).replace(/'/g, `'\\''`)}'`;
}

function shellDoubleQuote(s) {
  return `"${String(s).replace(/(["\\$`])/g, '\\$1')}"`;
}
