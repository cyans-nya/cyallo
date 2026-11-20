# Cyallo native tier timing samples

Host arch aarch64, OS macos. Release build via `cargo test --release --offline --locked --test zz_sample`. Wall-clock via std::time::Instant. Microseconds unless a column says ns.

## Table 1: compile cost vs size

Emit time per call, microseconds: 20 emits of the same prepared plan. Cell = median (first emit). Bytes = UnlinkedImage::bytes().len() of the first emit. The first row (Winch) is the first emit in the process.

| shape | statements | plan sites | Winch us | LL us | Opt us | Winch bytes | LL bytes | Opt bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| straight | 1 | 28 | 21.9 (517.1) | 703.9 (2141.6) | 760.6 (1087.0) | 2496 | 1996 | 2104 |
| straight | 4 | 82 | 39.4 (53.5) | 1515.7 (1904.5) | 1682.8 (1822.2) | 7260 | 6028 | 6148 |
| straight | 16 | 298 | 102.6 (113.2) | 4917.8 (5198.2) | 6163.6 (6312.3) | 26316 | 24520 | 27088 |
| straight | 64 | 1162 | 397.9 (444.0) | 25204.2 (25622.2) | 29940.2 (29889.9) | 102540 | 116752 | 114276 |
| straight | 256 | 4618 | 1682.5 (1596.4) | 178219.1 (177240.7) | 198504.2 (197058.8) | 407436 | 474092 | 461800 |
| loop | 1 | 67 | 23.2 (39.0) | 1120.7 (1330.7) | 1448.6 (1565.5) | 5868 | 6208 | 6308 |
| loop | 4 | 121 | 42.4 (48.7) | 2306.7 (2478.4) | 2885.7 (2960.1) | 10632 | 11416 | 11812 |
| loop | 16 | 337 | 117.7 (124.8) | 7619.6 (8075.6) | 9339.1 (9398.3) | 29688 | 37740 | 38428 |
| loop | 64 | 1201 | 419.5 (419.8) | 36074.1 (35699.1) | 41916.1 (41933.2) | 105912 | 149768 | 152212 |
| loop | 256 | 4657 | 1666.6 (1577.4) | 271569.7 (271389.3) | 295522.8 (302355.1) | 410808 | 595320 | 594676 |

## Table 2: run cost per instruction

Wall = activation start + set_output + resume to completion, median of 5 runs. ns/instr = median wall ns / instructions executed (interpreter count; native counts are checked equal). check = status, printed lines and instruction count against the interpreter reference.

| program | param | tier | instructions | wall us (median) | ns/instr | result | check |
|---|---:|---|---:|---:|---:|---|---|
| scalar sum loop | 131072 | interpreter | 3538964 | 27991.8 | 7.91 | I64(8590000128) | reference |
| scalar sum loop | 131072 | Winch | 3538964 | 2515.6 | 0.71 | I64(8590000128) | match |
| scalar sum loop | 131072 | Cranelift LowLatency | 3538964 | 796.8 | 0.23 | I64(8590000128) | match |
| scalar sum loop | 131072 | Cranelift Optimized | 3538964 | 790.3 | 0.22 | I64(8590000128) | match |
| float loop | 65536 | interpreter | 3538977 | 28771.7 | 8.13 | F64(4683743543745839104) | reference |
| float loop | 65536 | Winch | 3538977 | 2059.5 | 0.58 | F64(4683743543745839104) | match |
| float loop | 65536 | Cranelift LowLatency | 3538977 | 797.8 | 0.23 | F64(4683743543745839104) | match |
| float loop | 65536 | Cranelift Optimized | 3538977 | 795.1 | 0.22 | F64(4683743543745839104) | match |
| loop with scalar calls | 65536 | interpreter | 3997723 | 34805.4 | 8.71 | I64(32695176) | reference |
| loop with scalar calls | 65536 | Winch | 3997723 | 5790.9 | 1.45 | I64(32695176) | match |
| loop with scalar calls | 65536 | Cranelift LowLatency | 3997723 | 3413.8 | 0.85 | I64(32695176) | match |
| loop with scalar calls | 65536 | Cranelift Optimized | 3997723 | 3371.2 | 0.84 | I64(32695176) | match |
| CAIE fixed array (param = reps of 1000) | 64 | interpreter | 3113455 | 27037.8 | 8.68 | returned () | reference |
| CAIE fixed array (param = reps of 1000) | 64 | Winch | 3113455 | 2144.1 | 0.69 | returned () | match |
| CAIE fixed array (param = reps of 1000) | 64 | Cranelift LowLatency | 3113455 | 712.2 | 0.23 | returned () | match |
| CAIE fixed array (param = reps of 1000) | 64 | Cranelift Optimized | 3113455 | 695.9 | 0.22 | returned () | match |
| tuple/enum loop | 32768 | interpreter | 4030491 | 39539.0 | 9.81 | I64(268419072) | reference |
| tuple/enum loop | 32768 | Winch | 4030491 | 8099.3 | 2.01 | I64(268419072) | match |
| tuple/enum loop | 32768 | Cranelift LowLatency | 4030491 | 5477.3 | 1.36 | I64(268419072) | match |
| tuple/enum loop | 32768 | Cranelift Optimized | 4030491 | 5466.8 | 1.36 | I64(268419072) | match |
| loop with print every 1000 | 65536 | interpreter | 3867377 | 31276.0 | 8.09 | I64(3144630) | reference |
| loop with print every 1000 | 65536 | Winch | 3867377 | 2676.5 | 0.69 | I64(3144630) | match |
| loop with print every 1000 | 65536 | Cranelift LowLatency | 3867377 | 880.1 | 0.23 | I64(3144630) | match |
| loop with print every 1000 | 65536 | Cranelift Optimized | 3867377 | 895.0 | 0.23 | I64(3144630) | match |

## Table 3: slice overhead (scalar sum loop, param 131072, 3538964 instructions)

Overhead per resume = (median total - median single-slice total) / number of resume calls. Median of 5 runs. Start is included in total.

| tier | budget | resumes | total wall us (median) | overhead per resume ns | status check |
|---|---:|---:|---:|---:|---|
| Winch | single slice (budget 9223372036854775807) | 1 | 2418.2 | baseline | match |
| Winch | single slice (budget 1099511627776) | 1 | 2447.5 | baseline | match |
| Winch | 1000 | 3539 | 2606.8 | 45.0 | match |
| Winch | 10000 | 354 | 2498.0 | 142.8 | match |
| Winch | 50000 | 71 | 2516.7 | 974.2 | match |
| Winch | 100000 | 36 | 2512.4 | 1802.1 | match |
| Winch | 1000000 | 4 | 2508.4 | 15229.3 | match |
| Cranelift LowLatency | single slice (budget 9223372036854775807) | 1 | 788.3 | baseline | match |
| Cranelift LowLatency | single slice (budget 1099511627776) | 1 | 788.3 | baseline | match |
| Cranelift LowLatency | 1000 | 3539 | 830.5 | 11.9 | match |
| Cranelift LowLatency | 10000 | 354 | 798.7 | 29.3 | match |
| Cranelift LowLatency | 50000 | 71 | 789.6 | 18.2 | match |
| Cranelift LowLatency | 100000 | 36 | 788.9 | 15.0 | match |
| Cranelift LowLatency | 1000000 | 4 | 788.4 | 10.2 | match |

## Table 4: startup phases (microseconds, median of 10)

Each iteration uses a fresh compile, lower_shared(program, 0) and CodeStore. 'entry' rows time only routine 0; 'all' rows sum successful emit/publish over every function. Prepare is timed for the entry only (store.prepare(entry) in a fresh store).

| phase | hot-loop.ya | osr-loop.ya |
|---|---:|---:|
| compile | 3086.4 | 3232.7 |
| lower_shared(program, 0) | 35.2 | 32.6 |
| CodeStore::new | 0.1 | 0.1 |
| prepare entry | 4.1 | 3.4 |
| Winch: emit entry | 11.6 | 10.5 |
| Winch: publish entry | 8.8 | 8.4 |
| Winch: emit all | 45.1 | 44.6 |
| Winch: publish all | 24.5 | 19.3 |
| Cranelift LowLatency: emit entry | 345.2 | 334.9 |
| Cranelift LowLatency: publish entry | 7.8 | 7.9 |
| Cranelift LowLatency: emit all | 1684.6 | 1632.5 |
| Cranelift LowLatency: publish all | 20.6 | 20.3 |
| Cranelift Optimized: emit entry | 422.2 | 413.9 |
| Cranelift Optimized: publish entry | 7.3 | 8.9 |
| Cranelift Optimized: emit all | 2039.2 | 2033.6 |
| Cranelift Optimized: publish all | 19.9 | 21.2 |

All tiers published the entry.

## Table 5: sampling feasibility (Winch, scalar sum loop, param 131072)

resume(100000) repeatedly; after each Paused, status() is read. Per-call status() cost is the mean over 1000 calls in one batch.

| measure | value |
|---|---|
| Paused returns | 35 |
| status() is Paused after each pause | 35 / 35 |
| function id resolves in VerifiedModule | 35 / 35 |
| instruction index in range of function | 35 / 35 |
| PreparedFunction::site(instruction) found | 35 / 35 |
| distinct function ids seen | [0] |
| status() cost ns (median / min / max per pause) | 4.1 / 4.1 / 5.4 |
| first positions | fn 0 instr 42; fn 0 instr 59; fn 0 instr 28 |
| final status | I64(8590000128) |

## Send check

CodeStore: NOT Send (compile error E0277: its EntryPin holds Rc<Published>, reached through HashMap<FunctionId, EntryPin>); its check line was removed so the sampler could build. PreparedFunction and UnlinkedImage: Send (this file compiled with their checks).

