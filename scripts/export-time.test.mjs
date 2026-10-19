import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import { runInNewContext } from 'node:vm';

// Execute the same standalone script that Rust embeds in the exported HTML.
const script = readFileSync(new URL('../src-tauri/src/export-time.js', import.meta.url), 'utf8');

function renderTimes(values) {
  const elements = values.map((value) => ({
    dataset: { time: value },
    textContent: '时间未知',
    attributes: {},
    setAttribute(key, value) { this.attributes[key] = value; },
    removeAttribute(key) { delete this.attributes[key]; },
  }));
  const exportTime = { textContent: '' };
  runInNewContext(script, {
    document: {
      querySelector: (selector) => {
        assert.equal(selector, '#export-time');
        return exportTime;
      },
      querySelectorAll: (selector) => {
        assert.equal(selector, 'time[data-time]');
        return elements;
      },
    },
  });
  assert.notEqual(exportTime.textContent, '');
  return elements;
}

test('missing and invalid timestamps never become 1970 or Invalid Date', () => {
  for (const element of renderTimes(['0', '-1', '', ' ', undefined, 'NaN', 'oops', 'Infinity', '9223372036854775807'])) {
    assert.equal(element.textContent, '时间未知');
    assert.equal(element.attributes.datetime, undefined);
  }
});

test('known 2017, 2018 and 2019 timestamps retain their actual dates', () => {
  const dates = ['2017-06-15T12:34:56.000Z', '2018-09-14T13:49:00.000Z', '2019-11-25T12:52:00.000Z'];
  const elements = renderTimes(dates.map((date) => String(Date.parse(date) / 1000)));
  elements.forEach((element, index) => {
    assert.equal(element.attributes.datetime, dates[index]);
    assert.equal(element.textContent, new Date(dates[index]).toLocaleString());
  });
});

test('unknown post time does not prevent known comment and reply times rendering', () => {
  const [post, comment, reply] = renderTimes(['0', '1574686320', '1574686380']);
  assert.equal(post.textContent, '时间未知');
  assert.equal(comment.attributes.datetime, '2019-11-25T12:52:00.000Z');
  assert.equal(reply.attributes.datetime, '2019-11-25T12:53:00.000Z');
});
