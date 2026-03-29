import Mathlib
import OpenProof.Corpus

-- openproof: aime_1987_p5 :: (x y : ℤ)     (h₀ : y^2 + 3 * (x^2 * y^2) = 30 * x^2 + 517) :     3 * (x^2 * y^2) = 588

theorem aime_1987_p5
    (x y : ℤ)
    (h₀ : y^2 + 3 * (x^2 * y^2) = 30 * x^2 + 517) :
    3 * (x^2 * y^2) = 588 := by
  have hfact : (y^2 - 10) * (3 * x^2 + 1) = 507 := by
    nlinarith [h₀]
  have hxpos : 0 < 3 * x^2 + 1 := by
    have hxsq : 0 ≤ x^2 := by nlinarith [sq_nonneg x]
    omega
  have hygt : 0 < y^2 - 10 := by
    have : 0 < (y^2 - 10) * (3 * x^2 + 1) := by
      simp [hfact] using (show (0 : ℤ) < 507 by decide)
    nlinarith
  have hyge : 1 ≤ y^2 - 10 := by omega
  have hxsq_le : x^2 ≤ 168 := by
    nlinarith [hfact, hyge]
  have hx_upper : x ≤ 12 := by
    have hnonneg : 0 ≤ (x - 13)^2 := by nlinarith [sq_nonneg (x - 13)]
    nlinarith
  have hx_lower : -12 ≤ x := by
    have hnonneg : 0 ≤ (x + 13)^2 := by nlinarith [sq_nonneg (x + 13)]
    nlinarith
  have hdvd : 3 * x^2 + 1 ∣ 507 := by
    refine ⟨y^2 - 10, ?_⟩
    simpa [mul_comm] using hfact.symm
  interval_cases x <;> norm_num at hdvd
  · norm_num at h₀ ⊢
    nlinarith
  · norm_num at h₀ ⊢
    have hy_upper : y ≤ 22 := by
      nlinarith [h₀, sq_nonneg (y - 23)]
    have hy_lower : -22 ≤ y := by
      nlinarith [h₀, sq_nonneg (y + 23)]
    have h1 : 0 ≤ 22 - y := by omega
    have h2 : 0 ≤ 22 + y := by omega
    have hmul : 0 ≤ (22 - y) * (22 + y) := mul_nonneg h1 h2
    nlinarith
  · norm_num at h₀ ⊢
    nlinarith