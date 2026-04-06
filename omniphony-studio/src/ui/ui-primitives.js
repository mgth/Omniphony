export function panelHeader({ titleKey, titleText, summaryId = null, summaryText = '—', toggleId, infoButton = null }) {
  const infoMarkup = infoButton
    ? `<button id="${infoButton.id}" type="button" class="info-icon-btn" data-i18n-title="${infoButton.titleKey}" title="${infoButton.titleText}">i</button>`
    : '';
  const summaryMarkup = summaryId
    ? `<div id="${summaryId}" class="panel-summary" style="display:none">${summaryText}</div>`
    : '';
  return `
        <div class="panel-header">
          <div class="panel-header-main">
            <div class="panel-title-wrap">
              <div class="info-title panel-title" data-i18n="${titleKey}">${titleText}</div>
              ${summaryMarkup}
            </div>
            ${infoMarkup}
          </div>
          <button id="${toggleId}" type="button" class="panel-toggle-btn">▸</button>
        </div>`;
}

export function secondaryButton({ id, text, textKey = null, title = null, titleKey = null, compact = false, extraClass = '', type = 'button' }) {
  const textAttr = textKey ? ` data-i18n="${textKey}"` : '';
  const titleAttr = titleKey
    ? ` data-i18n-title="${titleKey}" title="${title || ''}"`
    : (title ? ` title="${title}"` : '');
  const className = ['ui-btn', compact ? 'ui-btn-compact' : '', extraClass].filter(Boolean).join(' ');
  return `<button id="${id}" type="${type}" class="${className}"${textAttr}${titleAttr}>${text}</button>`;
}

export function primaryButton({ id, text, textKey = null, type = 'button', extraClass = '' }) {
  const textAttr = textKey ? ` data-i18n="${textKey}"` : '';
  const className = ['ui-btn', 'ui-btn-primary', extraClass].filter(Boolean).join(' ');
  return `<button id="${id}" type="${type}" class="${className}"${textAttr}>${text}</button>`;
}
