import { localPath, localExpanded, localHome, setLocalPath, setLocalExpanded, setLocalHome } from './state.js';
import { toggleSidebar } from './terminal.js';

export async function loadLocalDir(dir) {
  const listEl = document.getElementById('filer-list');
  const pathEl = document.getElementById('filer-path');
  if (!listEl) return;

  if (!localHome) {
    const h = await window.rterm.getEnv('HOME');
    setLocalHome((h.result || '').trim() || '/');
  }

  const resolved = dir === '~' || dir === '' ? localHome : dir;
  setLocalExpanded(resolved);
  setLocalPath(dir);

  const result = await window.rterm.localList(resolved);
  if (!result.success) { listEl.innerHTML = '<div style="padding:8px;color:var(--red)">Error: ' + (result.error || 'unknown') + '</div>'; return; }

  const parts = dir.split('/').filter(Boolean);
  pathEl.innerHTML = parts.map((p, i) => {
    const path = '/' + parts.slice(0, i + 1).join('/');
    return `<span style="color:var(--text3)">/</span><span onclick="loadLocalDir('${path}')">${p}</span>`;
  }).join('');
  if (dir === '/') pathEl.innerHTML = '<span style="color:var(--text3)">/</span>';
  else if (dir === '~') pathEl.innerHTML = '<span style="color:var(--text3)">~</span>';

  listEl.innerHTML = '';

  if (dir !== '/' && dir !== '~') {
    const parentDiv = document.createElement('div');
    parentDiv.className = 'filer-item';
    parentDiv.innerHTML = '<span class="icon"><svg viewBox="0 0 24 24" fill="none" stroke="var(--text3)" stroke-width="1.5"><path d="M22 19a2 2 0 01-2 2H4a2 2 0 01-2-2V5a2 2 0 012-2h5l2 3h9a2 2 0 012 2z"></path></svg></span><span class="name" style="color:var(--text3)">..</span>';
    parentDiv.onclick = () => loadLocalDir(dir.split('/').filter(Boolean).slice(0, -1).join('/') || '/');
    listEl.appendChild(parentDiv);
  }

  const files = result.result || [];
  files.forEach(f => {
    const div = document.createElement('div');
    div.className = 'filer-item' + (f.dir ? ' selected' : '');
    const icon = f.dir
      ? '<svg viewBox="0 0 24 24" fill="none" stroke="var(--accent)" stroke-width="1.5"><path d="M22 19a2 2 0 01-2 2H4a2 2 0 01-2-2V5a2 2 0 012-2h5l2 3h9a2 2 0 012 2z"></path></svg>'
      : '<svg viewBox="0 0 24 24" fill="none" stroke="var(--text2)" stroke-width="1.5"><path d="M14 2H6a2 2 0 00-2 2v16a2 2 0 002 2h12a2 2 0 002-2V8z"></path><polyline points="14 2 14 8 20 8"></polyline></svg>';
    const sizeStr = f.size ? (f.size > 1024 * 1024 * 1024 ? (f.size / 1024 / 1024 / 1024).toFixed(1) + 'G' : f.size > 1024 * 1024 ? (f.size / 1024 / 1024).toFixed(1) + 'M' : f.size > 1024 ? (f.size / 1024).toFixed(1) + 'K' : f.size + 'B') : '';
    div.innerHTML = `<span class="icon">${icon}</span><span class="name" style="overflow:hidden;text-overflow:ellipsis;white-space:nowrap;max-width:160px">${f.name}</span>${sizeStr ? `<span style="margin-left:auto;padding-right:8px;color:var(--text3);font-size:11px">${sizeStr}</span>` : ''}`;
    if (f.dir) { div.onclick = () => loadLocalDir((dir === '/' ? '/' : dir + '/') + f.name); }
    div.oncontextmenu = (e) => {
      e.preventDefault();
      e.stopPropagation();
      const old = document.getElementById('local-ctx');
      if (old) old.remove();
      const menu = document.createElement('div');
      menu.id = 'local-ctx';
      menu.style.cssText = 'position:fixed;z-index:9999;background:var(--bg3);border:1px solid var(--border2);border-radius:6px;padding:4px;box-shadow:0 8px 24px rgba(0,0,0,.5);min-width:140px;font-size:12px;';
      menu.innerHTML = '<div class="dropdown-item" id="lctx-delete" style="color:var(--red)">Delete ' + (f.dir ? 'Folder' : 'File') + '</div>';
      document.body.appendChild(menu);
      menu.style.left = Math.min(e.clientX, window.innerWidth - 160) + 'px';
      menu.style.top = Math.min(e.clientY, window.innerHeight - 60) + 'px';
      document.getElementById('lctx-delete').onclick = async () => {
        menu.remove();
        if (!confirm('Delete "' + f.name + '"?')) return;
        const path = (localPath === '/' ? '' : localPath) + '/' + f.name;
        const result = await window.rterm.localDelete(path, f.dir);
        if (result.success) loadLocalDir(localPath);
        else alert('Delete failed: ' + (result.error || 'unknown'));
      };
      setTimeout(() => {
        document.addEventListener('click', () => { const m = document.getElementById('local-ctx'); if (m) m.remove(); }, { once: true });
      }, 10);
    };
    listEl.appendChild(div);
  });
}

window.loadLocalDir = loadLocalDir;
