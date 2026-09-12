const { getDefaultConfig, mergeConfig } = require('@react-native/metro-config');

/**
 * R21 — Metro 配置：允许 RN bundle 引用仓库级共享 core
 * （../src-tauri/frontend/src/remote/core）。
 */
const config = {};

module.exports = mergeConfig(getDefaultConfig(__dirname), {
  watchFolders: [`${__dirname}/../src-tauri/frontend/src/remote`],
  resolver: {
    sourceExts: ['tsx', 'ts', 'jsx', 'js', 'json'],
  },
});
