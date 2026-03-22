#!/usr/bin/env python3
"""Generate interactive HTML report from shredwatch JSON log."""

import json
import sys
from collections import defaultdict
from pathlib import Path

try:
    import plotly.graph_objects as go
    from plotly.subplots import make_subplots
except ImportError:
    print("Install plotly: pip install plotly")
    sys.exit(1)


def load_log(path):
    entries = []
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("//"):
                continue
            entries.append(json.loads(line))
    return entries


def build_report(entries, output_path):
    # Collect per-source latency data
    source_deltas = defaultdict(list)       # source -> [delta_ns]
    source_over_time = defaultdict(lambda: {"t": [], "delta": []})  # source -> {t[], delta[]}
    slot_wins = defaultdict(lambda: defaultdict(int))  # slot -> {source -> count}
    all_sources = set()

    for e in entries:
        t_ns = e["first_seen_ns"]
        slot = e["slot"]
        first_source = e["first_source"]
        slot_wins[slot][first_source] += 1

        for a in e["arrivals"]:
            src = a["source"]
            delta = a["delta_ns"]
            all_sources.add(src)
            source_deltas[src].append(delta)
            source_over_time[src]["t"].append(t_ns / 1e9)  # seconds
            source_over_time[src]["delta"].append(delta / 1e6)  # ms

    sources = sorted(all_sources)

    fig = make_subplots(
        rows=4, cols=1,
        subplot_titles=(
            "Latency Distribution (histogram)",
            "Latency Over Time (scatter)",
            "CDF — Cumulative Latency Distribution",
            "Per-Slot First Arrival Wins (stacked bar)",
        ),
        vertical_spacing=0.07,
        row_heights=[0.25, 0.25, 0.25, 0.25],
    )

    colors = [
        "#636EFA", "#EF553B", "#00CC96", "#AB63FA",
        "#FFA15A", "#19D3F3", "#FF6692", "#B6E880",
        "#FF97FF", "#FECB52",
    ]

    # 1) Histogram
    for i, src in enumerate(sources):
        deltas_ms = [d / 1e6 for d in source_deltas[src]]
        fig.add_trace(
            go.Histogram(
                x=deltas_ms,
                name=src,
                opacity=0.7,
                marker_color=colors[i % len(colors)],
                legendgroup=src,
                nbinsx=200,
            ),
            row=1, col=1,
        )
    fig.update_xaxes(title_text="Latency (ms)", row=1, col=1)
    fig.update_yaxes(title_text="Count", row=1, col=1)

    # 2) Scatter — latency over time
    for i, src in enumerate(sources):
        data = source_over_time[src]
        # Downsample if too many points for browser performance
        step = max(1, len(data["t"]) // 50_000)
        fig.add_trace(
            go.Scattergl(
                x=data["t"][::step],
                y=data["delta"][::step],
                mode="markers",
                marker=dict(size=2, color=colors[i % len(colors)], opacity=0.5),
                name=src,
                legendgroup=src,
                showlegend=False,
            ),
            row=2, col=1,
        )
    fig.update_xaxes(title_text="Time since start (s)", row=2, col=1)
    fig.update_yaxes(title_text="Latency (ms)", row=2, col=1)

    # 3) CDF
    for i, src in enumerate(sources):
        sorted_ms = sorted(d / 1e6 for d in source_deltas[src])
        n = len(sorted_ms)
        # Downsample CDF for performance
        step = max(1, n // 10_000)
        x = sorted_ms[::step]
        y = [(j * step + 1) / n * 100 for j in range(len(x))]
        fig.add_trace(
            go.Scatter(
                x=x,
                y=y,
                mode="lines",
                name=src,
                legendgroup=src,
                showlegend=False,
                line=dict(color=colors[i % len(colors)], width=2),
            ),
            row=3, col=1,
        )
    fig.update_xaxes(title_text="Latency (ms)", row=3, col=1)
    fig.update_yaxes(title_text="Percentile (%)", row=3, col=1)

    # 4) Per-slot stacked bar
    slots_sorted = sorted(slot_wins.keys())
    slot_labels = [str(s) for s in slots_sorted]
    for i, src in enumerate(sources):
        counts = [slot_wins[s].get(src, 0) for s in slots_sorted]
        fig.add_trace(
            go.Bar(
                x=slot_labels,
                y=counts,
                name=src,
                legendgroup=src,
                showlegend=False,
                marker_color=colors[i % len(colors)],
            ),
            row=4, col=1,
        )
    fig.update_xaxes(title_text="Slot", row=4, col=1)
    fig.update_yaxes(title_text="Shreds Won", row=4, col=1)

    # Build a color legend string like "● Source A  ● Source B  ● Source C"
    legend_parts = []
    for i, src in enumerate(sources):
        color = colors[i % len(colors)]
        legend_parts.append(f'<span style="color:{color}">●</span> {src}')
    legend_text = "&nbsp;&nbsp;&nbsp;".join(legend_parts)

    # Add color legend annotation above each subplot
    # Subplot title y-positions for 4 equal rows with 0.07 spacing
    subplot_y_positions = [1.0, 0.735, 0.47, 0.205]
    annotations = list(fig.layout.annotations)  # keep existing subplot titles
    for y_pos in subplot_y_positions:
        annotations.append(dict(
            text=legend_text,
            xref="paper", yref="paper",
            x=0.5, y=y_pos - 0.015,
            showarrow=False,
            font=dict(size=11),
            xanchor="center", yanchor="top",
        ))

    fig.update_layout(
        title="Shredwatch Report",
        height=2200,
        template="plotly_dark",
        legend=dict(orientation="h", yanchor="bottom", y=1.02, xanchor="center", x=0.5),
        barmode="stack",
        annotations=annotations,
    )

    fig.write_html(str(output_path), include_plotlyjs="cdn")
    print(f"Report written to {output_path}")


def main():
    if len(sys.argv) < 2:
        print(f"Usage: {sys.argv[0]} <shreds.jsonl> [output.html]")
        sys.exit(1)

    log_path = sys.argv[1]
    output_path = sys.argv[2] if len(sys.argv) > 2 else Path(log_path).with_suffix(".html")

    entries = load_log(log_path)
    print(f"Loaded {len(entries)} shred entries")
    build_report(entries, output_path)


if __name__ == "__main__":
    main()
