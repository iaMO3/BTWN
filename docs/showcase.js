const root = document.documentElement;
const tabs = Array.from(document.querySelectorAll('[role="tab"]'));
const tabList = tabs[0].parentElement;
const horizontal = window.matchMedia('(max-width: 740px)');
const reduced = window.matchMedia('(prefers-reduced-motion: reduce)');
const stage = document.querySelector('.art-stage');
const motionButton = document.querySelector('.motion');
const demoButton = document.querySelector('#run-demo');
const demoStatus = document.querySelector('#demo-status');
const percentages = Array.from(document.querySelectorAll('.percent'));
const meters = Array.from(document.querySelectorAll('.demo-row progress'));
const duration = 6400;

let accumulated = 0;
let started = null;
let frame = 0;
let lastValue = -1;
let motionPaused = reduced.matches;
try { motionPaused ||= localStorage.getItem('btwn-site-motion') === 'paused'; } catch { /* Storage is optional. */ }

function updateMotion() {
  const paused = motionPaused || reduced.matches;
  root.dataset.motion = paused ? 'off' : 'on';
  motionButton.hidden = reduced.matches;
  const label = paused ? 'Resume visual motion' : 'Pause visual motion';
  motionButton.setAttribute('aria-label', label);
  motionButton.title = label;
  motionButton.querySelector('use').setAttribute('href', paused ? '#play' : '#pause');
  if (paused) clearTilt();
}
function clearTilt() {
  stage.style.removeProperty('--tilt-x');
  stage.style.removeProperty('--tilt-y');
}
motionButton.addEventListener('click', () => {
  motionPaused = !motionPaused;
  try { localStorage.setItem('btwn-site-motion', motionPaused ? 'paused' : 'on'); } catch { /* Optional. */ }
  updateMotion();
});
reduced.addEventListener('change', updateMotion);
stage.addEventListener('pointermove', event => {
  if (root.dataset.motion === 'off' || event.pointerType !== 'mouse') return;
  const bounds = stage.getBoundingClientRect();
  stage.style.setProperty('--tilt-x', ((event.clientX - bounds.left) / bounds.width * 14 - 7) + 'px');
  stage.style.setProperty('--tilt-y', ((event.clientY - bounds.top) / bounds.height * 10 - 5) + 'px');
});
stage.addEventListener('pointerleave', clearTilt);

function paintDemo(value) {
  if (value === lastValue) return;
  lastValue = value;
  for (const meter of meters) meter.value = value;
  for (const label of percentages) label.textContent = value + '%';
}
function tick(now) {
  const elapsed = Math.min(duration, accumulated + now - started);
  paintDemo(Math.floor(elapsed / duration * 100));
  if (elapsed < duration) {
    frame = window.requestAnimationFrame(tick);
    return;
  }
  started = null;
  accumulated = duration;
  demoStatus.textContent = 'Both PCs confirmed';
  demoButton.textContent = 'Replay preview';
}
function pauseDemo() {
  if (started === null) return;
  accumulated = Math.min(duration, accumulated + performance.now() - started);
  cancelAnimationFrame(frame);
  started = null;
  paintDemo(Math.floor(accumulated / duration * 100));
  const done = accumulated >= duration;
  demoStatus.textContent = done ? 'Both PCs confirmed' : 'Paused';
  demoButton.textContent = done ? 'Replay preview' : 'Resume preview';
}
demoButton.addEventListener('click', () => {
  if (started !== null) { pauseDemo(); return; }
  if (accumulated >= duration) accumulated = 0;
  started = performance.now();
  demoButton.textContent = 'Pause preview';
  demoStatus.textContent = 'Transferring';
  tick(started);
});
document.addEventListener('visibilitychange', () => { if (document.hidden) pauseDemo(); });

function selectTab(tab, focus = false) {
  for (const candidate of tabs) {
    const selected = candidate === tab;
    candidate.setAttribute('aria-selected', String(selected));
    candidate.tabIndex = selected ? 0 : -1;
    document.getElementById(candidate.getAttribute('aria-controls')).hidden = !selected;
  }
  if (tab.id !== 'tab-transfer') pauseDemo();
  if (focus) tab.focus();
}
function updateOrientation() {
  tabList.setAttribute('aria-orientation', horizontal.matches ? 'horizontal' : 'vertical');
}
tabs.forEach((tab, index) => {
  tab.addEventListener('click', () => selectTab(tab));
  tab.addEventListener('keydown', event => {
    const next = horizontal.matches ? 'ArrowRight' : 'ArrowDown';
    const previous = horizontal.matches ? 'ArrowLeft' : 'ArrowUp';
    const targets = { [next]: (index + 1) % tabs.length, [previous]: (index + tabs.length - 1) % tabs.length, Home: 0, End: tabs.length - 1 };
    if (!(event.key in targets)) return;
    event.preventDefault();
    selectTab(tabs[targets[event.key]], true);
  });
});
horizontal.addEventListener('change', updateOrientation);
updateOrientation();
selectTab(tabs[0]);
updateMotion();
root.classList.add('enhanced');
