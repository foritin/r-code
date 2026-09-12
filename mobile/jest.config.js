/**
 * R21 — mobile jest 配置。
 *
 * 注意：早先的 testMatch 指向 `<rootDir>/../src-tauri/.../*.test.mjs`，
 * jest 实际根本搜不到（"No tests found" + CI 里 `--passWithNoTests`
 * 把零测试也判成通过）。现在只匹配 mobile 自身目录内的测试——
 * RN 侧真正要验证的是渲染与平台 adapter；core 的纯逻辑断言由
 * `node --test` 在仓库根目录跑（node 环境），两侧互不冒充。
 */
module.exports = {
  preset: 'react-native',
  rootDir: '.',
  testMatch: ['<rootDir>/__tests__/**/*.test.[jt]s?(x)'],
  transform: { '^.+\\.(js|mjs|ts|tsx)$': 'babel-jest' },
  moduleFileExtensions: ['ts', 'tsx', 'js', 'mjs', 'json'],
};
