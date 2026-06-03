#[doc = "Register `v_timing` reader"]
pub type R = crate::R<V_TIMING_SPEC>;
#[doc = "Register `v_timing` writer"]
pub type W = crate::W<V_TIMING_SPEC>;
#[doc = "Field `v_active` writer - v_active field"]
pub type V_ACTIVE_W<'a, REG> = crate::FieldWriter<'a, REG, 16, u16>;
#[doc = "Field `v_sync_start` writer - v_sync_start field"]
pub type V_SYNC_START_W<'a, REG> = crate::FieldWriter<'a, REG, 16, u16>;
impl W {
    #[doc = "Bits 0:15 - v_active field"]
    #[inline(always)]
    pub fn v_active(&mut self) -> V_ACTIVE_W<'_, V_TIMING_SPEC> {
        V_ACTIVE_W::new(self, 0)
    }
    #[doc = "Bits 16:31 - v_sync_start field"]
    #[inline(always)]
    pub fn v_sync_start(&mut self) -> V_SYNC_START_W<'_, V_TIMING_SPEC> {
        V_SYNC_START_W::new(self, 16)
    }
}
#[doc = "A CSR register. Parameters ---------- fields : :class:`dict` or :class:`list` or :class:`Field` Collection of register fields. If ``None`` (default), a dict is populated from Python :term:`variable annotations <python:variable annotations>`. ``fields`` is used to create a :class:`FieldActionMap`, :class:`FieldActionArray`, or :class:`FieldAction`, depending on its type (dict, list, or Field). Interface attributes -------------------- element : :class:`Element` Interface between this register and a CSR bus primitive. Attributes ---------- field : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Collection of field instances. f : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Shorthand for :attr:`Register.field`. Raises ------ :exc:`TypeError` If ``fields`` is neither ``None``, a :class:`dict`, a :class:`list`, or a :class:`Field`. :exc:`ValueError` If ``fields`` is not ``None`` and at least one variable annotation is a :class:`Field`. :exc:`ValueError` If ``element.access`` is not readable and at least one field is readable. :exc:`ValueError` If ``element.access`` is not writable and at least one field is writable.\n\nYou can [`read`](crate::Reg::read) this register and get [`v_timing::R`](R). You can [`reset`](crate::Reg::reset), [`write`](crate::Reg::write), [`write_with_zero`](crate::Reg::write_with_zero) this register using [`v_timing::W`](W). You can also [`modify`](crate::Reg::modify) this register. See [API](https://docs.rs/svd2rust/#read--modify--write-api)."]
pub struct V_TIMING_SPEC;
impl crate::RegisterSpec for V_TIMING_SPEC {
    type Ux = u32;
}
#[doc = "`read()` method returns [`v_timing::R`](R) reader structure"]
impl crate::Readable for V_TIMING_SPEC {}
#[doc = "`write(|w| ..)` method takes [`v_timing::W`](W) writer structure"]
impl crate::Writable for V_TIMING_SPEC {
    type Safety = crate::Unsafe;
}
#[doc = "`reset()` method sets v_timing to value 0"]
impl crate::Resettable for V_TIMING_SPEC {}
