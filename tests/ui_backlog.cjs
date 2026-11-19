// Offline UI checks with synthetic RPC replies; no desktop clients are opened.
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const vm = require('node:vm');
const assert = require('node:assert/strict');
const test = require('node:test');

const root = join(__dirname, '..');
function load(context, ...scripts) {
  for (const name of scripts)
    vm.runInContext(readFileSync(join(root, 'assets/ui/scripts', name + '.js'), 'utf8'), context);
}

function handlingFixture() {
  const elements = new Map();
  for (const selector of ['#handling-project', '#handling-project-note', '#handling-start-note', '#dialog [data-handle="submit"]', '#handling-instructions'])
    elements.set(selector, { value: '合成补充说明', classList: { toggle() {} } });
  const listeners = [];
  const context = vm.createContext({
    document: { addEventListener(type, handler) { listeners.push([type, handler]); } },
    addEventListener() {},
    $: (selector) => elements.get(selector), esc: String, icon: () => '',
    state: { handling: [] },
    setModal(html) { context.modal = html; },
    connections: [
      { id: 'codex', driver: 'codex', name: 'Codex', models: [] },
      { id: 'dsh', driver: 'dsh', name: 'DSH', models: [{ model: 'm', supportedReasoningEfforts: [{ reasoningEffort: 'high' }] }] },
    ],
    projects: [],
    async rpc(method, params) {
      if (method === 'handling_prepare') return { connections: context.connections, prompt: '合成提示词', suggested: '' };
      assert.equal(method, 'handling_projects');
      context.projects.push(params.connection_id);
      return [{ id: params.connection_id + ':p', name: '合成项目', cwd: 'C:/synthetic' }];
    },
  });
  load(context, 'handling');
  context.click = listeners.find(([type]) => type === 'click')[1];
  context.choose = (connection, model = '') => {
    vm.runInContext(`handlingMenu = { kind: 'model', host: { firstElementChild: {} }, input: { value: '' }, el: { querySelector() { return {}; } } }`, context);
    context.click({ target: { closest(selector) {
      return selector === '.handling-menu button' ? { dataset: { connection, model } } : null;
    } } });
  };
  return context;
}

test('client preference stays in memory and only selects an available client', async () => {
  const c = handlingFixture();
  await vm.runInContext('beginHandling("task")', c);
  assert.equal(c.projects.at(-1), 'codex');
  assert.match(vm.runInContext('handlingMenuItems("model", null, "")', c), /data-connection="dsh"/);
  c.choose('dsh', 'm');
  await vm.runInContext('beginHandling("task")', c);
  assert.equal(c.projects.at(-1), 'dsh');
  assert.equal(vm.runInContext('handlingChoices.pick.model', c), '', 'only the client is remembered');
  assert.match(c.modal, /DSH/);
  c.connections = c.connections.filter((connection) => connection.driver === 'codex');
  await vm.runInContext('beginHandling("task")', c);
  assert.equal(c.projects.at(-1), 'codex');
  assert.doesNotMatch(vm.runInContext('handlingMenuItems("model", null, "")', c), /DSH|dsh/);
  c.connections = [];
  const count = c.projects.length;
  await assert.rejects(vm.runInContext('beginHandling("task")', c), /请先在设置中添加/);
  assert.equal(c.projects.length, count);
  const reopened = handlingFixture();
  await vm.runInContext('beginHandling("task")', reopened);
  assert.equal(reopened.projects.at(-1), 'codex');
});

test('switching clients refreshes projects and preserves the instruction draft', async () => {
  const c = handlingFixture();
  await vm.runInContext('beginHandling("task")', c);
  c.choose('dsh', 'm');
  await new Promise(setImmediate);
  assert.equal(vm.runInContext('handlingChoices.project', c), 'dsh:p');
  assert.equal(vm.runInContext('$("#handling-instructions").value', c), '合成补充说明');
  assert.match(vm.runInContext('handlingChip(handlingChoices.pick)', c), /DSH/);
  assert.match(vm.runInContext('handlingMenuItems("model", null, "")', c), /推理强度/);
  assert.match(vm.runInContext('handlingChip({...handlingChoices.pick, effort:"high"})', c), /<small>high<\/small>/);
  assert.match(vm.runInContext('handlingMenuItems("model", null, "")', c), /data-effort="high"[^>]*>high<\/button>/);
  c.choose('codex');
  await new Promise(setImmediate);
  assert.equal(vm.runInContext('handlingChoices.project', c), 'codex:p');
  assert.doesNotMatch(vm.runInContext('handlingChip(handlingChoices.pick)', c), /默认模型|<small>/);
  assert.doesNotMatch(vm.runInContext('handlingMenuItems("model", null, "")', c), /推理强度/);
});

test('settings embeds system prompts before storage and omits the handling model route', async () => {
  const c = vm.createContext({
    document: { addEventListener() {} }, page: 'settings', esc: String,
    state: { config: { codex: '', ai_connections: [], model_profiles: { review: { reasoning_effort: 'max' } }, leaders: [] } },
    closeConnectionEditor() {}, tagPriorityFields: () => '', retentionSummary: () => '', aboutSection: () => '',
    readSettingsValues: () => ({}), syncUpdateControls() {},
    renderSources() {}, renderPromptHistory() {}, renderMemory() {},
    setHtml(selector, html) { assert.equal(selector, '#view'); c.html = html; },
  });
  load(c, 'settings', 'model-routes');
  vm.runInContext('loadModels = () => {}; syncAutomationControls = () => {}', c);
  await vm.runInContext('renderSettings()', c);
  const settings = c.html;
  assert.ok(settings.indexOf('data-settings-target="system-prompts"') < settings.indexOf('data-settings-target="storage"'));
  assert.ok(settings.indexOf('id="settings-system-prompts"') < settings.indexOf('id="settings-storage"'));
  assert.match(settings, /data-settings-target="system-prompts"[\s\S]*data-settings-target="memory"[\s\S]*data-settings-target="storage"/);
  assert.match(settings, /id="settings-system-prompts"[\s\S]*id="settings-memory"[\s\S]*id="settings-storage"/);
  assert.match(settings, /id="settings-prompt-content" data-preserve-content/);
  assert.doesNotMatch(settings, /data-page="prompts"/);
  assert.match(settings, /data-model-profile="analysis"/);
  assert.match(settings, /data-model-profile="review"/);
  assert.match(settings, /value="max">max<\/option>/);
  assert.match(settings, /data-model-profile="filter"/);
  assert.doesNotMatch(settings, /cfg-handling|data-model-profile="handling"/);
  const effort = { value: 'max' };
  c.$ = (id) => id === '#cfg-review-effort' ? effort : { value: 'm' };
  vm.runInContext(`profileCatalogs.review = [{ model: 'm', supportedReasoningEfforts: [
    { reasoningEffort: 'max' }, { reasoningEffort: 'ultra' }] }]; updateEfforts(true, 'cfg-review-model')`, c);
  assert.equal(effort.value, 'max');
  assert.equal(effort.innerHTML, '<option value="">default</option><option value="max">max</option><option value="ultra">ultra</option>');
});

test('scope clicks combine and remove filters across the active and archive lists', async () => {
  const listeners = [], requests = [];
  const c = vm.createContext({
    document: { addEventListener(type, fn) { if (type === 'click') listeners.push(fn); } },
    window: { addEventListener() {} }, $: () => ({ addEventListener() {} }), closeBacklog() {}, toast: assert.fail,
    taskScopes: ['待处理'], query: '合成', quickView: '', taskFocusViews: {},
    tagSelection: () => ({ 类型: ['查询'] }), prioritySelection: () => ['高'], structuredClone,
  });
  load(c, 'task-feed', 'events');
  c.renderTasks = () => requests.push(c.taskFeedRequest());
  for (const tab of ['处理中', '归档', '待处理', '处理中', '归档'])
    await listeners[0]({ target: { closest: () => ({ dataset: { tab }, closest: () => null }) } });
  const results = JSON.parse(JSON.stringify(requests));
  assert.deepEqual(results.map(([method, p]) => [method, p.scope]), [
    ['active_tasks_query', ['待处理', '处理中']], ['active_tasks_query', ['待处理', '处理中', '归档']],
    ['active_tasks_query', ['处理中', '归档']], ['archived_tasks_query', ['归档']], ['active_tasks_query', []],
  ]);
  for (const [, p] of results) {
    assert.equal(p.query, '合成');
    assert.deepEqual(p.tag_filters, { 类型: ['查询'] });
    assert.deepEqual(p.priorities, ['高']);
  }
});

test('a saved focus restores its own filters and a renamed default keeps its scope', async () => {
  const events = {};
  const filters = { scope: ['归档', '被阻塞'], query: '合成', view: '', tag_filters: { 类型: ['核对'] }, priorities: ['高'] };
  const c = vm.createContext({
    window: { addEventListener() {} }, document: { addEventListener(type, fn) { events[type] ??= fn; } },
    $: () => ({ addEventListener() {} }), closeBacklog() {}, toast: assert.fail, leaveSettings: async () => true,
    render() {}, renderFocusViews() {}, saveTagFilters() {}, tagSelection: () => ({}), prioritySelection: () => [], structuredClone,
    state: { focus_views: [{ id: 'custom', name: '需核对', filters }, { id: '待处理', name: '待跟进' }] },
  });
  load(c, 'events');
  const select = (view) => events.click({ target: { closest: () => ({ dataset: { view }, closest: () => null }) } });
  await select('custom');
  assert.equal(c.page, 'tasks');
  assert.deepEqual(c.taskScopes, filters.scope);
  assert.equal(c.query, filters.query);
  assert.deepEqual(c.selectedTags, filters.tag_filters);
  assert.deepEqual(c.selectedPriorities, filters.priorities);
  c.selectedTags.类型.pop();
  assert.deepEqual(filters.tag_filters.类型, ['核对']);
  await select('待处理');
  assert.deepEqual(Array.from(c.taskScopes), ['待处理']);
});

test('manual filters and sidebar clicks select one matching focus and keep zero counts', async () => {
  const events = {}, html = new Map();
  const filters = { scope: ['被阻塞', '待处理'], view: '', query: '', tag_filters: {}, priorities: [] };
  const c = vm.createContext({
    window: { addEventListener() {} }, document: { addEventListener(type, fn) { events[type] = fn; } },
    $: () => ({ addEventListener() {} }), setHtml: (s, v) => html.set(s, v), clearHtml() {}, shanghaiDay: () => '', esc: String,
    page: 'tasks', selected: '', activeFocus: null, taskScopes: ['待处理'], quickView: '', query: '',
    tagSelection: () => ({}), prioritySelection: () => [], structuredClone, toast: assert.fail, leaveSettings: async () => true,
    pageFeed: () => ({ page: 1, load: async () => {}, dispose() {}, paint() {} }),
    state: { tag_state: { groups: [] }, task_focus_counts: { 待处理: 0, 被阻塞: 1, a: 1, b: 1 },
      focus_views: [{ id: '待处理', name: '待处理' }, { id: '被阻塞', name: '被阻塞' }, { id: 'a', name: '组合甲', filters }, { id: 'b', name: '组合乙', filters }] },
  });
  load(c, 'tasks', 'task-feed', 'events');
  vm.runInContext('closeBacklog = renderTaskList = () => {}; render = renderTasks', c);
  const sidebar = () => html.get('#sidebar-context');
  const select = (view) => events.click({ target: { closest: () => ({ dataset: { view }, closest: () => null }) } });
  await c.renderTasks();
  assert.match(sidebar(), /data-view="待处理" aria-current="true"[^>]*><span[^>]*>待处理<\/span><span>0</);
  const tab = (tab) => events.click({ target: { closest: () => ({ dataset: { tab }, closest: () => null }) } });
  await tab('待处理');
  assert.doesNotMatch(sidebar(), /aria-current="true"/);
  await tab('被阻塞');
  assert.match(sidebar(), /data-view="被阻塞" aria-current="true"/);
  await tab('待处理');
  await tab('被阻塞');
  await tab('被阻塞');
  assert.deepEqual(Array.from(c.taskScopes), ['待处理', '被阻塞']);
  assert.match(sidebar(), /data-view="a" aria-current="true"/);
  await select('a');
  await select('b');
  assert.match(sidebar(), /data-view="a" aria-current="false"/);
  assert.match(sidebar(), /data-view="b" aria-current="true"/);
  c.state.focus_views.find((v) => v.id === 'b').name = '改名后';
  c.state.task_focus_counts.b = 0;
  c.renderFocusViews();
  assert.match(sidebar(), /data-view="b" aria-current="true"[^>]*><span[^>]*>改名后<\/span><span>0</);
  c.page = 'settings';
  c.renderFocusViews();
  assert.doesNotMatch(sidebar(), /aria-current="true"/);
  c.page = 'tasks';
  c.query = '手动筛选';
  await c.renderTasks();
  assert.doesNotMatch(sidebar(), /aria-current="true"/);
  const tagged = { ...filters, query: '  ALPHA', priorities: ['高', '普通'], tag_filters: { 组甲: ['甲', '乙'], 组乙: ['丙'] } };
  c.state.focus_views.push({ id: 'tags', name: '带标签', filters: tagged });
  c.state.task_focus_counts.tags = 0;
  c.tagSelection = () => ({ 组乙: ['丙'], 组甲: ['乙', '甲'] });
  c.prioritySelection = () => ['普通', '高'];
  c.query = 'alpha';
  await c.renderTasks();
  assert.match(sidebar(), /data-view="tags" aria-current="true"/);
  c.state.focus_views = c.state.focus_views.filter((v) => v.id !== 'tags');
  c.renderFocusViews();
  assert.doesNotMatch(sidebar(), /aria-current="true"/);
});

test('dragging a focus requests one move to the indicated side of the target', async () => {
  const calls = [], store = new Map(), list = {};
  const c = vm.createContext({ $: () => list, document: { addEventListener() {} }, window: { addEventListener() {} },
    rpc: async (method, args) => calls.push([method, args]), refreshState() {}, toast: assert.fail });
  load(c, 'task-multi');
  const from = { dataset: { view: '被阻塞' } }, to = { dataset: { view: 'custom' }, getBoundingClientRect: () => ({ top: 20, height: 32 }) };
  const transfer = { get types() { return [...store.keys()]; }, setData: (k, v) => store.set(k, v), getData: (k) => store.get(k) };
  const event = (row, y) => ({ target: { closest: () => row }, clientY: y, dataTransfer: transfer, preventDefault() {} });
  list.ondrop(event(to, 25));
  list.ondragstart(event(from));
  for (const y of [25, 48]) {
    list.ondragover(event(to, y));
    assert.equal(to.dataset.drop, y === 25 ? 'before' : 'after');
    list.ondrop(event(to, y));
    assert.equal(to.dataset.drop, undefined);
  }
  assert.deepEqual(JSON.parse(JSON.stringify(calls)), [
    ['focus_view_edit', { action: 'move', id: '被阻塞', target: 'custom', after: false }],
    ['focus_view_edit', { action: 'move', id: '被阻塞', target: 'custom', after: true }],
  ]);
});

test('tool library renders a page from the state snapshot and filters without a request', () => {
  const count = {}, html = new Map();
  const c = vm.createContext({
    document: { addEventListener() {} }, esc: String,
    $: (selector) => { assert.equal(selector, '#tool-count'); return count; },
    setHtml: (selector, content) => html.set(selector, content),
    empty: (title, message) => `${title} ${message}`,
    state: { config: { tools: [
      { id: 'a', name: 'query_items', display_name: '查询事项', description: '按关键字查询合成事项', enabled: true, parameters: [{ name: 'keyword' }] },
      { id: 'b', name: 'read_notes', display_name: '读取笔记', description: '读取合成笔记', enabled: false, parameters: [] },
    ] } },
  });
  load(c, 'tools');
  vm.runInContext('showToolLibrary()', c);
  assert.match(html.get('#view'), /class="page tool-library"/);
  assert.doesNotMatch(html.get('#view'), /dialog-foot|data-action="close"/);
  assert.match(html.get('#tool-list-rows'), /data-tool-edit="a"/);
  assert.match(html.get('#tool-list-rows'), /aria-checked="true"/);
  assert.equal(count.textContent, '2 / 2个工具');
  vm.runInContext('toolQuery = "笔记"; renderToolRows()', c);
  assert.doesNotMatch(html.get('#tool-list-rows'), /data-tool-edit="a"/);
  assert.match(html.get('#tool-list-rows'), /data-tool-edit="b"/);
  assert.equal(count.textContent, '1 / 2个工具');
  vm.runInContext('toolQuery = "不存在"; renderToolRows()', c);
  assert.match(html.get('#tool-list-rows'), /没有匹配的工具/);
  c.state.config.tools = [];
  vm.runInContext('renderToolRows()', c);
  assert.match(html.get('#tool-list-rows'), /还没有工具/);
});
