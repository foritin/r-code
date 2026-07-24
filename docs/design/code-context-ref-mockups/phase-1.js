const stageButtons = [...document.querySelectorAll('[data-stage]')];
const selectionAction = document.getElementById('selection-action');
const contextShelf = document.getElementById('context-shelf');
const contextCard = document.getElementById('context-card');
const composerInput = document.getElementById('composer-input');
const sendButton = document.getElementById('send-button');
const sentEntry = document.getElementById('sent-entry');
const agentFollowup = document.getElementById('agent-followup');
const sentCopy = document.getElementById('sent-copy');
const conversationScroll = document.getElementById('conversation-scroll');
const workingCopy = document.getElementById('working-copy');
const toast = document.getElementById('toast');
let toastTimer;

function showToast(message) {
  toast.textContent = message;
  toast.classList.add('visible');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => toast.classList.remove('visible'), 1900);
}

function updateSend() {
  sendButton.disabled =
    !composerInput.value.trim() || !contextShelf.classList.contains('visible');
}

function setStage(stage, userInitiated = false) {
  stageButtons.forEach((button) =>
    button.classList.toggle('active', button.dataset.stage === stage),
  );
  selectionAction.hidden = stage !== 'selection';
  contextShelf.classList.toggle('visible', stage === 'attached');
  sentEntry.classList.toggle('hidden', stage !== 'sent');
  agentFollowup.classList.toggle('hidden', stage !== 'sent');

  if (stage === 'selection') {
    composerInput.value = '';
    workingCopy.textContent = '等待你的审阅上下文…';
  }

  if (stage === 'attached') {
    if (!composerInput.value.trim()) {
      composerInput.value = '把这两个颜色调整得更符合现在的品牌色。';
    }
    workingCopy.textContent = '上下文已加入草稿，尚未发送';
    requestAnimationFrame(() => composerInput.focus());
    if (userInitiated) showToast('已添加 public/index.html L16–17；补充要求后发送。');
  }

  if (stage === 'sent') {
    const text = composerInput.value.trim() || '把这两个颜色调整得更符合现在的品牌色。';
    sentCopy.textContent = text;
    composerInput.value = '';
    workingCopy.textContent = 'Agent 正在根据这 2 行代码继续工作…';
    requestAnimationFrame(() => {
      conversationScroll.scrollTop = conversationScroll.scrollHeight;
    });
    if (userInitiated) showToast('消息已发送；引用快照已固化到 Session 时间线。');
  }

  updateSend();
}

stageButtons.forEach((button) =>
  button.addEventListener('click', () => setStage(button.dataset.stage)),
);
document
  .getElementById('attach-selection')
  .addEventListener('click', () => setStage('attached', true));
document.getElementById('remove-context').addEventListener('click', () => {
  setStage('selection');
  showToast('已从草稿中移除代码引用。');
});
contextCard.addEventListener('click', (event) => {
  if (event.target.closest('button')) return;
  contextCard.classList.toggle('expanded');
});
composerInput.addEventListener('input', updateSend);
composerInput.addEventListener('keydown', (event) => {
  if (event.key === 'Enter' && !event.shiftKey && !sendButton.disabled) {
    event.preventDefault();
    setStage('sent', true);
  }
});
sendButton.addEventListener('click', () => setStage('sent', true));
setStage('selection');
