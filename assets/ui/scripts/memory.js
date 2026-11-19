let memoryData = null,
  memoryEditor = null,
  memoryKind = 'temporary',
  memoryRequest = 0;
async function renderMemory() {
  const request = ++memoryRequest,
    epoch = renderEpoch;
  const result = await rpc('memory_read');
  if (page !== 'settings' || request !== memoryRequest || epoch !== renderEpoch) return;
  memoryData = result;
  if (!memoryEditor || !memoryEditor.saving) drawMemory();
}
function memoryTextCount() {
  return [...memoryEditor.text].length;
}
function drawMemory() {
  const data = memoryData[memoryKind],
    text = memoryEditor ? memoryEditor.text : data.text,
    count = [...text].length,
    overLimit = count > data.max_chars,
    temporary = memoryKind === 'temporary';
  setHtml(
    '#settings-memory-content',
    `<div class="row memory-toolbar"><div class="pills" role="group" aria-label="记忆类型"><button data-memory-kind="temporary" aria-pressed="${temporary}" class="${temporary ? 'active' : ''}">临时记忆</button><button data-memory-kind="archived" aria-pressed="${!temporary}" class="${temporary ? '' : 'active'}">归档记忆</button></div>${help(temporary ? '分析之间的交接笔记，归档时并入归档记忆后清空。' : '归档时结合操作记录和临时记忆，整理长期经验和偏好。')}<span id="memory-count" class="helper${overLimit ? ' memory-over-limit' : ''}">${count} / ${data.max_chars} 字符</span>${memoryEditor ? '<button data-memory-cancel>取消</button><button class="primary" data-memory-save>保存记忆</button>' : '<button class="quiet" data-memory-reload>重新读取</button><button data-memory-edit>编辑记忆</button>'}</div><div data-preserve-content><textarea id="memory-text" aria-label="记忆正文" readonly></textarea></div><p id="memory-limit" class="helper memory-over-limit"${overLimit ? '' : ' hidden'}>原内容已完整保留，请精简到 ${data.max_chars} 字符以内再保存。</p><div id="memory-error" class="error-box" role="status"></div>`,
  );
  const field = $('#memory-text');
  field.readOnly = !memoryEditor;
  field.placeholder = temporary ? '还没有临时记忆，AI会在分析时按需记录。' : '还没有归档记忆。';
  if (field.value !== text) {
    const top = field.scrollTop;
    field.value = text;
    field.scrollTop = top;
  }
}
function leaveMemory() {
  if (!memoryEditor) return true;
  if (memoryEditor.saving) {
    toast('记忆正在保存，请稍候');
    return false;
  }
  if (!confirm('记忆修改尚未保存，放弃本次编辑？')) return false;
  memoryEditor = null;
  return true;
}
document.addEventListener('input', (e) => {
  if (e.target.id !== 'memory-text') return;
  memoryEditor.text = e.target.value;
  const count = memoryTextCount(),
    limit = memoryData[memoryKind].max_chars;
  $('#memory-count').textContent = `${count} / ${limit} 字符`;
  $('#memory-count').classList.toggle('memory-over-limit', count > limit);
  $('#memory-limit').hidden = count <= limit;
});
document.addEventListener('click', async (e) => {
  const b = e.target.closest('button');
  if (!b || ![...b.attributes].some((a) => a.name.startsWith('data-memory-'))) return;
  try {
    if (b.dataset.memoryKind) {
      if (memoryKind === b.dataset.memoryKind || !leaveMemory()) return;
      memoryKind = b.dataset.memoryKind;
      drawMemory();
      $('#memory-text').scrollTop = 0;
      return;
    }
    if (b.hasAttribute('data-memory-edit')) {
      const data = memoryData[memoryKind];
      memoryEditor = {
        expected: data.revision,
        text: data.text,
      };
      drawMemory();
      return;
    }
    if (b.hasAttribute('data-memory-cancel')) {
      memoryEditor = null;
      drawMemory();
      return;
    }
    if (b.hasAttribute('data-memory-reload')) {
      await renderMemory();
      return;
    }
    if (b.hasAttribute('data-memory-save')) {
      if (memoryEditor.saving) return;
      const limit = memoryData[memoryKind].max_chars;
      if (memoryTextCount() > limit) return;
      memoryEditor.saving = true;
      document
        .querySelectorAll('.memory-page button,.memory-page textarea')
        .forEach((el) => (el.disabled = true));
      try {
        const { expected, text } = memoryEditor,
          result = await rpc('memory_save', { kind: memoryKind, expected, text });
        ++memoryRequest;
        memoryData = result;
        memoryEditor = null;
        drawMemory();
        toast('记忆已保存');
      } catch (error) {
        $('#memory-error').textContent = error.message;
      } finally {
        if (memoryEditor) memoryEditor.saving = false;
        document
          .querySelectorAll('.memory-page button,.memory-page textarea')
          .forEach((el) => (el.disabled = false));
      }
    }
  } catch (error) {
    toast(error.message);
  }
});
