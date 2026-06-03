#[doc = "Register `hv_timing` reader"]
pub type R = crate::R<HV_TIMING_SPEC>;
#[doc = "Register `hv_timing` writer"]
pub type W = crate::W<HV_TIMING_SPEC>;
#[doc = "Field `h_sync_invert` writer - h_sync_invert field"]
pub type H_SYNC_INVERT_W<'a, REG> = crate::BitWriter<'a, REG>;
#[doc = "Field `v_sync_invert` writer - v_sync_invert field"]
pub type V_SYNC_INVERT_W<'a, REG> = crate::BitWriter<'a, REG>;
#[doc = "Field `active_pixels` writer - active_pixels field"]
pub type ACTIVE_PIXELS_W<'a, REG> = crate::FieldWriter<'a, REG, 30, u32>;
impl W {
    #[doc = "Bit 0 - h_sync_invert field"]
    #[inline(always)]
    pub fn h_sync_invert(&mut self) -> H_SYNC_INVERT_W<'_, HV_TIMING_SPEC> {
        H_SYNC_INVERT_W::new(self, 0)
    }
    #[doc = "Bit 1 - v_sync_invert field"]
    #[inline(always)]
    pub fn v_sync_invert(&mut self) -> V_SYNC_INVERT_W<'_, HV_TIMING_SPEC> {
        V_SYNC_INVERT_W::new(self, 1)
    }
    #[doc = "Bits 2:31 - active_pixels field"]
    #[inline(always)]
    pub fn active_pixels(&mut self) -> ACTIVE_PIXELS_W<'_, HV_TIMING_SPEC> {
        ACTIVE_PIXELS_W::new(self, 2)
    }
}
#[doc = "A CSR register. Parameters ---------- fields : :class:`dict` or :class:`list` or :class:`Field` Collection of register fields. If ``None`` (default), a dict is populated from Python :term:`variable annotations <python:variable annotations>`. ``fields`` is used to create a :class:`FieldActionMap`, :class:`FieldActionArray`, or :class:`FieldAction`, depending on its type (dict, list, or Field). Interface attributes -------------------- element : :class:`Element` Interface between this register and a CSR bus primitive. Attributes ---------- field : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Collection of field instances. f : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Shorthand for :attr:`Register.field`. Raises ------ :exc:`TypeError` If ``fields`` is neither ``None``, a :class:`dict`, a :class:`list`, or a :class:`Field`. :exc:`ValueError` If ``fields`` is not ``None`` and at least one variable annotation is a :class:`Field`. :exc:`ValueError` If ``element.access`` is not readable and at least one field is readable. :exc:`ValueError` If ``element.access`` is not writable and at least one field is writable.\n\nYou can [`read`](crate::Reg::read) this register and get [`hv_timing::R`](R). You can [`reset`](crate::Reg::reset), [`write`](crate::Reg::write), [`write_with_zero`](crate::Reg::write_with_zero) this register using [`hv_timing::W`](W). You can also [`modify`](crate::Reg::modify) this register. See [API](https://docs.rs/svd2rust/#read--modify--write-api)."]
pub struct HV_TIMING_SPEC;
impl crate::RegisterSpec for HV_TIMING_SPEC {
    type Ux = u32;
}
#[doc = "`read()` method returns [`hv_timing::R`](R) reader structure"]
impl crate::Readable for HV_TIMING_SPEC {}
#[doc = "`write(|w| ..)` method takes [`hv_timing::W`](W) writer structure"]
impl crate::Writable for HV_TIMING_SPEC {
    type Safety = crate::Unsafe;
}
#[doc = "`reset()` method sets hv_timing to value 0"]
impl crate::Resettable for HV_TIMING_SPEC {}
