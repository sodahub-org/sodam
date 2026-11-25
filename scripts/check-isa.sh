#!/usr/bin/env bash
# 发布产物指令集护栏：x86_64 包里 ring 的 x25519 C 函数不许出现 AVX2 / BMI2 / AVX-512。
#
# 背景（#306）：ubuntu-26.04 的 runner 默认装 amd64v3 变体的 GCC（--with-arch-64=x86-64-v3），
# C 依赖会被编成 x86-64-v3。没有 AVX2/BMI2 的老 CPU（Ivy Bridge 等）一发起 HTTPS 就在
# TLS 握手里 SIGILL，进程内拦不住，只能在产物这一层挡。
#
# 只查编译器生成的 C 函数。ring 里由 CPUID 守着、运行时才走的 `*_adx` 汇编本来就用
# BMI2/ADX，一并查就是误报。
#
# 用法：scripts/check-isa.sh <elf>
set -euo pipefail

binary=${1:?usage: check-isa.sh <elf>}
[[ -f "$binary" ]] || {
    echo "找不到产物：$binary" >&2
    exit 1
}

if ! readelf -h "$binary" | grep -q 'X86-64'; then
    echo "跳过 $binary：不是 x86-64 产物"
    exit 0
fi

# 后缀匹配，顺带把 `_adx` 汇编变体排除在外
symbols=$(nm "$binary" 2>/dev/null | awk 'NF >= 3 { print $NF }' |
    grep -E 'x25519_(ge_scalarmult_base|public_from_private_generic_masked)$' || true)

if [[ -z "$symbols" ]]; then
    echo "在 $binary 里没找到 ring 的 x25519 C 函数（被 strip 了？），护栏失效" >&2
    exit 1
fi

status=0
while IFS= read -r symbol; do
    hits=$(objdump -d --disassemble="$symbol" "$binary" |
        grep -E '\b(mulx|adcx|adox|rorx)\b|%ymm|%zmm' || true)
    if [[ -n "$hits" ]]; then
        echo "越界：$symbol 里有高于 x86-64 baseline 的指令（#306）" >&2
        echo "$hits" | head -n 5 >&2
        status=1
    fi
done <<<"$symbols"

if [[ $status -ne 0 ]]; then
    echo "产物要求 AVX2/BMI2：老 CPU 会在 TLS 握手时 SIGILL。检查 runner 镜像和 CFLAGS。" >&2
fi

exit $status
