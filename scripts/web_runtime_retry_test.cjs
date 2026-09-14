// Requires Playwright in NODE_PATH and web_runtime_retry_fixture.mjs running.
const { chromium } = require('playwright');
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const path = require('node:path');

(async () => {
  const base = 'http://127.0.0.1:19849';
  const output = path.resolve('target/web-runtime-retry');
  await fs.mkdir(output, { recursive: true });
  const browser = await chromium.launch({ channel: 'chrome', headless: true });
  const checks = [];
  try {
    const page = await browser.newPage({ viewport: { width: 1440, height: 1000 }, locale: 'zh-CN' });
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    const phase = async value => {
      const response = await page.request.post(`${base}/__qa/phase?value=${value}`);
      assert.equal(response.status(), 200);
    };
    await phase('waiting');
    const initial = await (await page.request.get(`${base}/__qa/state`)).json();
    const stopCount = value => value.requests.filter(item => item.method === 'POST' && item.path.endsWith('/qa-child/stop')).length;
    const initialStops = stopCount(initial);
    await page.goto(base);
    await page.getByRole('button', { name: '展开', exact: true }).click();
    await page.getByText('等待重试', { exact: false }).waitFor();
    assert.equal(await page.getByRole('button', { name: '停止生成', exact: true }).isEnabled(), true);
    checks.push('waiting status rendered and stop enabled');
    await page.reload();
    await page.getByRole('button', { name: '展开', exact: true }).click();
    await page.getByText('等待重试', { exact: false }).waitFor();
    await page.screenshot({ path: path.join(output, 'waiting-after-refresh.png'), fullPage: true });
    checks.push('fresh page restores waiting status from activity API');
    await phase('running');
    await page.getByText('等待重试', { exact: false }).waitFor({ state: 'hidden' });
    checks.push('polling clears retry wait after retry starts');
    await phase('waiting');
    await page.getByText('等待重试', { exact: false }).waitFor();
    await page.getByRole('button', { name: '停止生成', exact: true }).click();
    await page.getByText('等待重试', { exact: false }).waitFor({ state: 'hidden' });
    const state = await (await page.request.get(`${base}/__qa/state`)).json();
    assert.equal(state.phase, 'stopped');
    assert.equal(stopCount(state) - initialStops, 1);
    await page.reload();
    await page.getByRole('button', { name: '展开', exact: true }).click();
    await page.getByRole('button', { name: '重试', exact: true }).waitFor();
    assert.equal(await page.getByRole('button', { name: '停止生成', exact: true }).count(), 0);
    await page.screenshot({ path: path.join(output, 'stopped-after-refresh.png'), fullPage: true });
    checks.push('stop sends one scoped request and terminal state survives refresh');
    assert.deepEqual(errors, []);
    const report = { passed: true, scope: 'production React App with local mock Runtime API', checks, page_errors: errors };
    await fs.writeFile(path.join(output, 'report.json'), JSON.stringify(report, null, 2));
    await fs.writeFile(path.join(output, 'report.md'), '# Web 重试状态回归\n\n' + checks.map(check => `- PASS: ${check}`).join('\n') + '\n');
    console.log(JSON.stringify(report));
  } finally { await browser.close(); }
})().catch(error => { console.error(error); process.exitCode = 1; });
