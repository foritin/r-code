/**
 * R26 — 原生设置/诊断屏（原型屏④）：消费共享 core 投影
 * （core/settings-diagnostics.ts 的 settingsModel），RN 侧不复制任何投影逻辑。
 *
 * 能力行不可点击（授予在桌面端）；清除本机令牌走两步确认；诊断日志与中继
 * 地址展开后用 selectable 文本（系统长按复制菜单）——不引入额外原生依赖。
 */
import React from "react";
import {
  ScrollView,
  StyleSheet,
  Text,
  TouchableOpacity,
  View,
} from "react-native";
import type {
  SettingsAction,
  SettingsRow,
  SettingsSection,
} from "../../../src-tauri/frontend/src/remote/core/settings-diagnostics.ts";

export interface SettingsScreenProps {
  sections: SettingsSection[];
  onAction: (action: SettingsAction) => void;
}

const colors = {
  background: "#181818",
  surface: "#202020",
  border: "#343434",
  text: "#eeeeee",
  muted: "#b4b4b4",
  accent: "#f4742b",
  danger: "#ff6b6b",
};

export function SettingsScreen({ sections, onAction }: SettingsScreenProps): React.JSX.Element {
  const [expandedId, setExpandedId] = React.useState<string | null>(null);
  const [confirmClear, setConfirmClear] = React.useState(false);

  function pressRow(row: SettingsRow): void {
    const action = row.action;
    if (!action) return;
    switch (action.kind) {
      case "copy":
        setExpandedId(expandedId === row.id ? null : row.id);
        return;
      case "clear-token":
        if (confirmClear) {
          setConfirmClear(false);
          onAction(action);
        } else {
          setConfirmClear(true);
        }
        return;
      case "switch-strategy":
        onAction(action);
        return;
    }
  }

  function rowValue(row: SettingsRow): string {
    if (row.action?.kind === "clear-token" && confirmClear) {
      return "再点一次确认清除（不可撤销）";
    }
    return row.value;
  }

  return (
    <ScrollView style={styles.container} contentContainerStyle={styles.content}>
      {sections.map((section) => (
        <View key={section.id} style={styles.section}>
          <Text style={styles.sectionTitle}>{section.title}</Text>
          {section.rows.map((row) => {
            const interactive = row.action !== null;
            const expanded = expandedId === row.id && row.action?.kind === "copy";
            const body = (
              <View style={styles.rowBody}>
                <Text style={styles.rowLabel}>{row.label}</Text>
                <Text
                  style={[
                    styles.rowValue,
                    expanded && styles.rowValueExpanded,
                    row.action?.kind === "clear-token" && confirmClear && styles.rowValueDanger,
                  ]}
                  selectable={expanded}
                >
                  {rowValue(row)}
                </Text>
                {row.hint ? <Text style={styles.rowHint}>{row.hint}</Text> : null}
              </View>
            );
            if (!interactive) {
              return (
                <View key={row.id} style={styles.row}>
                  {body}
                </View>
              );
            }
            return (
              <TouchableOpacity
                key={row.id}
                style={styles.row}
                onPress={() => pressRow(row)}
                accessibilityRole="button"
                accessibilityLabel={`${row.label} ${rowValue(row)}`}
              >
                {body}
              </TouchableOpacity>
            );
          })}
        </View>
      ))}
    </ScrollView>
  );
}

const styles = StyleSheet.create({
  container: { flex: 1, backgroundColor: colors.background },
  content: { paddingBottom: 24 },
  section: { paddingHorizontal: 16, paddingTop: 16 },
  sectionTitle: {
    color: colors.accent,
    fontSize: 13,
    fontWeight: "600",
    letterSpacing: 0.4,
    paddingBottom: 8,
  },
  row: {
    backgroundColor: colors.surface,
    borderBottomWidth: StyleSheet.hairlineWidth,
    borderBottomColor: colors.border,
    paddingHorizontal: 16,
    paddingVertical: 12,
    minHeight: 48,
    justifyContent: "center",
  },
  rowBody: { flexShrink: 1 },
  rowLabel: { color: colors.text, fontSize: 15 },
  rowValue: { color: colors.muted, fontSize: 13, paddingTop: 2 },
  rowValueExpanded: { color: colors.text, fontFamily: "monospace" },
  rowValueDanger: { color: colors.danger },
  rowHint: { color: colors.muted, fontSize: 12, paddingTop: 4, opacity: 0.85 },
});

export default SettingsScreen;
