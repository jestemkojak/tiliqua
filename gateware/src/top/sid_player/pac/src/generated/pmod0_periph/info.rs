#[doc = "Register `info` reader"]
pub type R = crate::R<INFO_SPEC>;
#[doc = "Register `info` writer"]
pub type W = crate::W<INFO_SPEC>;
#[doc = "Field `f_bits` reader - f_bits field"]
pub type F_BITS_R = crate::FieldReader;
#[doc = "Field `counts_per_mv` reader - counts_per_mv field"]
pub type COUNTS_PER_MV_R = crate::FieldReader<u16>;
impl R {
    #[doc = "Bits 0:7 - f_bits field"]
    #[inline(always)]
    pub fn f_bits(&self) -> F_BITS_R {
        F_BITS_R::new((self.bits & 0xff) as u8)
    }
    #[doc = "Bits 8:23 - counts_per_mv field"]
    #[inline(always)]
    pub fn counts_per_mv(&self) -> COUNTS_PER_MV_R {
        COUNTS_PER_MV_R::new(((self.bits >> 8) & 0xffff) as u16)
    }
}
impl W {}
#[doc = "A CSR register. Parameters ---------- fields : :class:`dict` or :class:`list` or :class:`Field` Collection of register fields. If ``None`` (default), a dict is populated from Python :term:`variable annotations <python:variable annotations>`. ``fields`` is used to create a :class:`FieldActionMap`, :class:`FieldActionArray`, or :class:`FieldAction`, depending on its type (dict, list, or Field). Interface attributes -------------------- element : :class:`Element` Interface between this register and a CSR bus primitive. Attributes ---------- field : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Collection of field instances. f : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Shorthand for :attr:`Register.field`. Raises ------ :exc:`TypeError` If ``fields`` is neither ``None``, a :class:`dict`, a :class:`list`, or a :class:`Field`. :exc:`ValueError` If ``fields`` is not ``None`` and at least one variable annotation is a :class:`Field`. :exc:`ValueError` If ``element.access`` is not readable and at least one field is readable. :exc:`ValueError` If ``element.access`` is not writable and at least one field is writable.\n\nYou can [`read`](crate::Reg::read) this register and get [`info::R`](R). You can [`reset`](crate::Reg::reset), [`write`](crate::Reg::write), [`write_with_zero`](crate::Reg::write_with_zero) this register using [`info::W`](W). You can also [`modify`](crate::Reg::modify) this register. See [API](https://docs.rs/svd2rust/#read--modify--write-api)."]
pub struct INFO_SPEC;
impl crate::RegisterSpec for INFO_SPEC {
    type Ux = u32;
}
#[doc = "`read()` method returns [`info::R`](R) reader structure"]
impl crate::Readable for INFO_SPEC {}
#[doc = "`write(|w| ..)` method takes [`info::W`](W) writer structure"]
impl crate::Writable for INFO_SPEC {
    type Safety = crate::Unsafe;
}
#[doc = "`reset()` method sets info to value 0"]
impl crate::Resettable for INFO_SPEC {}
