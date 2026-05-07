const byIdCache = new Map();

export function $(selector, root = document) {
  return root.querySelector(selector);
}

export function $$(selector, root = document) {
  return Array.from(root.querySelectorAll(selector));
}

export function byId(id) {
  const cached = byIdCache.get(id);
  if (cached?.isConnected) {
    return cached;
  }

  const element = document.getElementById(id);
  byIdCache.set(id, element);
  return element;
}

export function on(target, event, handler, options) {
  target?.addEventListener(event, handler, options);
}

export function setDisplay(target, value) {
  if (target) target.style.display = value;
}

export function setText(target, value) {
  if (target) target.textContent = value;
}

export function setValue(target, value) {
  if (target) target.value = value;
}

export function setToggle(target, enabled) {
  target?.classList.toggle('on', Boolean(enabled));
}

export function showOnlyById(visibilityById) {
  Object.entries(visibilityById).forEach(([id, visible]) => {
    setDisplay(byId(id), visible ? 'block' : 'none');
  });
}

export function clearNode(target) {
  if (target) target.textContent = '';
}
