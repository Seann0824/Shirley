import { useCallback, useEffect, useState } from "react";

import { api } from "../api/bridge";
import type { Bot, InstalledSkill, MarketSkill, SkillMarket } from "../api/types";
import { BackHeader, ErrorNote, LoadingRows } from "../components/Chrome";
import { Icon } from "../components/Icon";

/**
 * One bot's skills: what it has, and the market to add more from. A skill installs into this
 * bot only; skills the bot wrote for itself are listed but never offered back to the market.
 */
export function Skills({ bot, onBack }: { bot: Bot; onBack: () => void }) {
  const [data, setData] = useState<SkillMarket | null>(null);
  const [error, setError] = useState<string | null>(null);
  /** Skill id -> what is happening to it right now. */
  const [busy, setBusy] = useState<Record<string, "installing" | "removing">>({});

  const load = useCallback(() => {
    api
      .skillMarket(bot.name)
      .then((value) => {
        setData(value);
        setError(null);
      })
      .catch((reason: unknown) => setError(reason instanceof Error ? reason.message : String(reason)));
  }, [bot.name]);

  useEffect(load, [load]);

  const act = async (id: string, kind: "installing" | "removing") => {
    setBusy((current) => ({ ...current, [id]: kind }));
    setError(null);
    try {
      if (kind === "installing") await api.installSkill(bot.name, id);
      else await api.uninstallSkill(bot.name, id);
      load();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setBusy((current) => {
        const next = { ...current };
        delete next[id];
        return next;
      });
    }
  };

  const installed = new Set(data?.installed.map((skill) => skill.id) ?? []);
  const titles = new Map(data?.market.map((skill) => [skill.id, skill]) ?? []);
  const offered = (data?.market ?? []).filter((skill) => !installed.has(skill.id));
  const categories = [...new Set(offered.map((skill) => skill.category || "其他"))];

  return (
    <>
      <BackHeader title={`${bot.display_name} 的技能`} onBack={onBack} />
      {error && <ErrorNote message={error} />}
      {!data && !error && <LoadingRows />}
      {data && (
        <div className="skills-page">
          <section>
            <h3>已安装</h3>
            {data.installed.length === 0 && <p className="empty-note">还没有技能。从下面的市场里装一个。</p>}
            {data.installed.map((skill) => (
              <InstalledRow
                key={skill.id}
                skill={skill}
                market={skill.market ? titles.get(skill.market) : undefined}
                busy={busy[skill.id]}
                onRemove={() => void act(skill.id, "removing")}
              />
            ))}
          </section>
          {categories.map((category) => (
            <section key={category}>
              <h3>{category}</h3>
              {offered
                .filter((skill) => (skill.category || "其他") === category)
                .map((skill) => (
                  <MarketRow
                    key={skill.id}
                    skill={skill}
                    busy={busy[skill.id]}
                    onInstall={() => void act(skill.id, "installing")}
                  />
                ))}
            </section>
          ))}
          <p className="skills-source">技能来自公开仓库（如 anthropics/skills），装到这个 Bot 里，只有它能用。</p>
        </div>
      )}
    </>
  );
}

function InstalledRow({
  skill,
  market,
  busy,
  onRemove,
}: {
  skill: InstalledSkill;
  market?: MarketSkill;
  busy?: "installing" | "removing";
  onRemove: () => void;
}) {
  return (
    <article className="skill-row">
      <span className="skill-icon">
        <Icon name="spark" size={17} />
      </span>
      <div>
        <strong>{market?.title ?? skill.id}</strong>
        <p>{market?.summary ?? skill.description}</p>
        {!skill.market && <small>Bot 自己写的</small>}
      </div>
      <button className="skill-action quiet" onClick={onRemove} disabled={Boolean(busy)} aria-label={`移除 ${skill.id}`}>
        {busy === "removing" ? "移除中…" : "移除"}
      </button>
    </article>
  );
}

function MarketRow({
  skill,
  busy,
  onInstall,
}: {
  skill: MarketSkill;
  busy?: "installing" | "removing";
  onInstall: () => void;
}) {
  return (
    <article className="skill-row">
      <span className="skill-icon">
        <Icon name="box" size={17} />
      </span>
      <div>
        <strong>{skill.title}</strong>
        <p>{skill.summary}</p>
      </div>
      <button className="skill-action" onClick={onInstall} disabled={Boolean(busy)} aria-label={`安装 ${skill.title}`}>
        {busy === "installing" ? "安装中…" : "安装"}
      </button>
    </article>
  );
}
