/* S1 spike: shared helpers (timestamps, stats). Throwaway code. */
#pragma once
#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <time.h>

static inline uint64_t mono_ns(void)
{
	struct timespec ts;
	clock_gettime(CLOCK_MONOTONIC, &ts);
	return (uint64_t)ts.tv_sec * 1000000000ull + (uint64_t)ts.tv_nsec;
}

static int cmp_d(const void *a, const void *b)
{
	double x = *(const double *)a, y = *(const double *)b;
	return x < y ? -1 : x > y;
}

/* Print min / mean / p50 / p99 / p99.9 / max / std of v[0..n) (copied, sorted). */
static void stats_line(const char *label, const double *v, size_t n, const char *unit)
{
	if (n == 0) {
		printf("  %-34s n=0\n", label);
		return;
	}
	double *s = malloc(n * sizeof(double));
	double sum = 0, sq = 0;
	for (size_t i = 0; i < n; i++) {
		s[i] = v[i];
		sum += v[i];
	}
	double mean = sum / n;
	for (size_t i = 0; i < n; i++)
		sq += (v[i] - mean) * (v[i] - mean);
	qsort(s, n, sizeof(double), cmp_d);
#define P(q) s[(size_t)((q) * (n - 1))]
	printf("  %-34s n=%zu min=%.3f mean=%.3f p50=%.3f p99=%.3f p99.9=%.3f max=%.3f std=%.3f %s\n",
	       label, n, s[0], mean, P(0.5), P(0.99), P(0.999), s[n - 1], sqrt(sq / n), unit);
#undef P
	free(s);
}

/* Least-squares fit y = a + b*x; returns residual std and peak-to-peak via out params. */
static void linfit(const double *x, const double *y, size_t n, double *a, double *b,
		   double *res_std, double *res_pp)
{
	double mx = 0, my = 0, sxx = 0, sxy = 0;
	for (size_t i = 0; i < n; i++) {
		mx += x[i];
		my += y[i];
	}
	mx /= n;
	my /= n;
	for (size_t i = 0; i < n; i++) {
		sxx += (x[i] - mx) * (x[i] - mx);
		sxy += (x[i] - mx) * (y[i] - my);
	}
	*b = sxx > 0 ? sxy / sxx : 0;
	*a = my - *b * mx;
	double sq = 0, lo = INFINITY, hi = -INFINITY;
	for (size_t i = 0; i < n; i++) {
		double r = y[i] - (*a + *b * x[i]);
		sq += r * r;
		if (r < lo) lo = r;
		if (r > hi) hi = r;
	}
	*res_std = n ? sqrt(sq / n) : 0;
	*res_pp = n ? hi - lo : 0;
}
