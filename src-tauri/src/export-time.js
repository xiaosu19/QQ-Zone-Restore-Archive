// Archive timestamps are Unix seconds. Zero means the source did not retain
// a reliable time; it must not be rendered as the Unix epoch (1970).
document.querySelector('#export-time').textContent = new Date().toLocaleString();
document.querySelectorAll('time[data-time]').forEach((element) => {
  const seconds = Number(element.dataset.time);
  const date = new Date(seconds * 1000);
  if (!Number.isFinite(seconds) || seconds <= 0 || !Number.isFinite(date.getTime())) {
    element.textContent = '时间未知';
    element.removeAttribute('datetime');
    return;
  }
  element.textContent = date.toLocaleString();
  element.setAttribute('datetime', date.toISOString());
});
