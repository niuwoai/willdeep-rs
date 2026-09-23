"""Quick local trial of Laya on Apple Silicon: accuracy on zh/en tickets + latency on MPS vs CPU."""
import statistics
import sys
import time

import torch
from laya import Router, triage_questions

WARMUP_RUNS = 3
TIMED_RUNS = 20

SAMPLES = [
    ("zh", "我这个月被扣了两次款，赶紧给我退一笔，不然我就去投诉你们！"),
    ("zh", "你好，想问一下企业版的价格是多少？支持开发票吗？"),
    ("zh", "你们的 API 从今天早上开始一直返回 500，我们线上业务全挂了，今天下午之前必须解决。"),
    ("zh", "用得不太满意，下个月准备换成别家了，怎么取消订阅？"),
    ("en", "I was charged twice this month, please refund one of them ASAP."),
    ("en", "Your API has been returning 500 errors since this morning and production is down."),
]


def summarize(result):
    """Keep only answer + confidence per question for readable output."""
    out = {}
    for name, ans in result["answers"].items():
        qtype = ans["type"]
        if qtype == "choice":
            value = ans["choice"]
        elif qtype == "score":
            value = f"{ans['score']:.2f}/3"
        elif qtype == "noul":
            value = f"yes={ans['noul']:.2f}"
        else:
            value = str(ans.get(qtype))
        out[name] = f"{value}  (conf {ans['confidence']:.2f})"
    return out


def bench(router, text, runs=TIMED_RUNS):
    questions = triage_questions()
    for _ in range(WARMUP_RUNS):
        router.predict({"message": text}, questions)
    times = []
    for _ in range(runs):
        t = time.perf_counter()
        router.predict({"message": text}, questions)
        if torch.backends.mps.is_available():
            torch.mps.synchronize()
        times.append((time.perf_counter() - t) * 1000)
    return statistics.median(times), max(times)


def main():
    device = sys.argv[1] if len(sys.argv) > 1 else None
    t0 = time.perf_counter()
    router = Router(device=device, max_loaded=2)
    router.preload(["english", "multilingual"])
    load_s = time.perf_counter() - t0
    agent = router.load("multilingual")
    print(f"device={agent.device} dtype={agent.dtype} load={load_s:.1f}s")

    questions = triage_questions()
    for lang, text in SAMPLES:
        res = router.predict({"message": text}, questions)
        print(f"\n[{lang} -> {res['routing'].get('model')}] {text}")
        for k, v in summarize(res).items():
            print(f"   {k:18s} {v}")

    for lang, text in (SAMPLES[0], SAMPLES[4]):
        p50, worst = bench(router, text)
        print(f"latency {lang}: p50={p50:.1f}ms max={worst:.1f}ms ({TIMED_RUNS} runs, 5 questions/call)")


if __name__ == "__main__":
    main()
