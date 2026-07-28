import {
  CaretDown,
  CheckCircle,
  DotsThree,
  Export,
  ShareNetwork,
  X,
} from "@phosphor-icons/react";
import { formatDuration, scenarios } from "../data.js";

const depthCopy = {
  recap: ["回顾", "先看结果"],
  explore: ["探索", "沿时间追问"],
  verify: ["核验", "查看证据"],
};

export function Header({ scenario, depth, onDepth, menuOpen, onMenu, onScenario, onToast }) {
  return (
    <>
      <header className="app-header">
        <div className="brand-mark" aria-label="Charter Replay">
          R
        </div>
        <button className="session-title" onClick={onMenu} aria-expanded={menuOpen}>
          <strong>{scenario.title}</strong>
          <span>
            Today, 10:14 · {formatDuration(scenario.actualSeconds)} actual · {formatDuration(scenario.recapSeconds)} recap
          </span>
          <CaretDown size={14} />
        </button>
        <nav className="depth-nav" aria-label="Replay depth">
          {Object.entries(depthCopy).map(([id, copy], index) => (
            <button
              key={id}
              className={depth === id ? "active" : ""}
              onClick={() => onDepth(id)}
              aria-current={depth === id ? "page" : undefined}
            >
              <span>{index + 1}</span>
              <strong>{copy[0]}</strong>
              <small>{copy[1]}</small>
            </button>
          ))}
        </nav>
        <div className="header-actions">
          <button className="quiet-button" onClick={() => onToast("证据凭证已准备，可复制安全链接。") }>
            <ShareNetwork size={16} />
            分享
          </button>
          <button className="icon-button" aria-label="More options" onClick={() => onToast("更多操作：复制任务链接、导出 JSON、打开原任务。") }>
            <DotsThree size={19} weight="bold" />
          </button>
          <button className="icon-button" aria-label="Close replay" onClick={() => onToast("这是原型：正式产品中会关闭回放并回到任务房间。") }>
            <X size={17} />
          </button>
        </div>
        {menuOpen ? (
          <div className="scenario-menu">
            <div className="scenario-menu-head">
              <span>切换演示任务</span>
              <small>同一套 Replay，三种 Agent 工作</small>
            </div>
            {scenarios.map((item) => (
              <button
                key={item.id}
                className={item.id === scenario.id ? "active" : ""}
                onClick={() => onScenario(item.id)}
              >
                <span className={`scenario-symbol symbol-${item.id}`}>{item.shortTitle.slice(0, 1)}</span>
                <span>
                  <strong>{item.shortTitle}</strong>
                  <small>{item.title}</small>
                </span>
                {item.id === scenario.id ? <CheckCircle size={18} weight="fill" /> : <Export size={16} />}
              </button>
            ))}
          </div>
        ) : null}
      </header>
      <section className="session-contract" aria-label="Session contract">
        <div className="contract-goal">
          <span>原始目标</span>
          <strong>{scenario.goal}</strong>
        </div>
        <div className="contract-fact">
          <span>结果</span>
          <strong className={`outcome-${scenario.outcome}`}>
            <CheckCircle size={15} weight="fill" />
            {scenario.outcomeLabel}
          </strong>
        </div>
        <div className="contract-fact">
          <span>验证</span>
          <strong>{scenario.verification}</strong>
        </div>
        <div className="contract-fact coverage-fact">
          <span>证据覆盖</span>
          <strong>{scenario.source}</strong>
          <div className="mini-coverage" aria-label="Evidence coverage by source">
            {scenario.coverage.map((part, index) => (
              <i key={`${part.level}-${index}`} className={`coverage-${part.level}`} style={{ width: `${part.width}%` }} />
            ))}
          </div>
        </div>
      </section>
    </>
  );
}
